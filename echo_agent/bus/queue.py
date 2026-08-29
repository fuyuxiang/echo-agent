"""Async message bus with pub/sub for event routing."""

from __future__ import annotations

import asyncio
from collections import defaultdict
from typing import TYPE_CHECKING, Awaitable, Callable

from loguru import logger

from echo_agent.bus.delivery import DeliveryResult, DeliveryStage
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.rate_limiter import SessionRateLimiter

if TYPE_CHECKING:
    from echo_agent.channels.base import SendResult

InboundHandler = Callable[[InboundEvent], Awaitable[None]]
InboundRejectionHandler = Callable[[InboundEvent, str], Awaitable[None]]
OutboundHandler = Callable[[OutboundEvent], Awaitable["SendResult | None"]]


class MessageBus:
    """Central event bus that decouples channels from the agent loop.

    Channels publish inbound events; the agent loop subscribes.
    The agent loop publishes outbound events; channels subscribe.
    """

    def __init__(self, max_queue_size: int = 1000, max_concurrency: int = 50):
        self._inbound_queue: asyncio.Queue[InboundEvent] = asyncio.Queue(maxsize=max_queue_size)
        self._outbound_handlers: dict[str, list[OutboundHandler]] = defaultdict(list)
        self._global_outbound_handlers: list[OutboundHandler] = []
        self._inbound_subscribers: list[InboundHandler] = []
        self._inbound_rejection_handlers: list[InboundRejectionHandler] = []
        self._running = False
        self._accepting = True
        self._dispatch_task: asyncio.Task | None = None
        self._inflight_inbound: set[asyncio.Task] = set()
        self._inflight_events: dict[asyncio.Task, InboundEvent] = {}
        self._lifecycle_lock = asyncio.Lock()
        # Admission has a separate short-held lock: publish_inbound must not
        # hold the lifecycle lock while back-pressured on queue.put(), but stop
        # still needs an atomic barrier against a publisher that observed
        # accepting=True and has not enqueued yet.
        self._admission_lock = asyncio.Lock()
        self._active_publishers = 0
        self._publishers_idle = asyncio.Event()
        self._publishers_idle.set()
        self._concurrency_sem = asyncio.Semaphore(max_concurrency)
        self._rate_limiter: SessionRateLimiter | None = None
        self._turn_run_store = None

    def set_rate_limiter(self, limiter: SessionRateLimiter) -> None:
        self._rate_limiter = limiter

    def set_turn_run_store(self, store) -> None:
        """Expose the durable ingress ledger through a narrow bus-owned seam."""
        self._turn_run_store = store

    async def get_turn_run(self, event_id: str) -> dict | None:
        store = self._turn_run_store
        if store is None:
            return None
        return await store.get(event_id)

    async def release_turn_acceptance(self, event_id: str, session_key: str) -> bool | None:
        store = self._turn_run_store
        if store is None:
            return None
        return await store.release_acceptance(event_id, session_key)

    async def claim_durable_idempotency(
        self,
        event_id: str,
        *,
        namespace: str,
        fingerprint: str,
        session_key: str,
    ) -> dict:
        store = self._turn_run_store
        if store is None:
            raise RuntimeError("durable idempotency storage is unavailable")
        return await store.claim_idempotency(
            event_id,
            namespace=namespace,
            fingerprint=fingerprint,
            session_key=session_key,
        )

    async def mark_durable_idempotency_admitted(self, event_id: str) -> None:
        store = self._turn_run_store
        if store is None:
            raise RuntimeError("durable idempotency storage is unavailable")
        await store.mark_idempotency_admitted(event_id)

    async def release_durable_idempotency(self, event_id: str) -> bool:
        store = self._turn_run_store
        if store is None:
            raise RuntimeError("durable idempotency storage is unavailable")
        return await store.release_idempotency(event_id)

    async def sync_durable_idempotency(self, row: dict) -> None:
        store = self._turn_run_store
        if store is None:
            raise RuntimeError("durable idempotency storage is unavailable")
        await store.sync_idempotency_from_turn(row)

    async def publish_inbound(self, event: InboundEvent) -> bool:
        async with self._admission_lock:
            if not self._accepting:
                admitted = False
            else:
                admitted = True
                self._active_publishers += 1
                self._publishers_idle.clear()
        if not admitted:
            logger.warning("Bus is shutting down, rejecting event from {}:{}", event.channel, event.chat_id)
            return False
        try:
            await asyncio.wait_for(self._inbound_queue.put(event), timeout=5.0)
            return True
        except asyncio.TimeoutError:
            logger.error("Inbound queue full after 5s wait, rejecting event from {}:{}", event.channel, event.chat_id)
            return False
        finally:
            async with self._admission_lock:
                self._active_publishers -= 1
                if self._active_publishers == 0:
                    self._publishers_idle.set()

    async def publish_outbound(self, event: OutboundEvent) -> DeliveryResult:
        if not self._global_outbound_handlers and event.channel not in self._outbound_handlers:
            logger.warning("No outbound handler for channel={}", event.channel)
            return DeliveryResult(DeliveryStage.NO_HANDLER, event.channel)

        # Snapshot handler lists before iterating so a handler that mutates the list
        # (e.g. subscribes/unsubscribes) won't trigger "list changed size" errors.
        global_handlers = list(self._global_outbound_handlers)
        global_results = await asyncio.gather(
            *(handler(event) for handler in global_handlers),
            return_exceptions=True,
        )
        agg = self._aggregate(event.channel, global_results)

        if event.metadata.get("_drop"):
            return agg

        specific_handlers = list(self._outbound_handlers.get(event.channel, []))
        if specific_handlers:
            specific_results = await asyncio.gather(
                *(handler(event) for handler in specific_handlers),
                return_exceptions=True,
            )
            agg = self._merge(agg, self._aggregate(event.channel, specific_results))
        return agg

    def _aggregate(self, channel: str, results: list) -> DeliveryResult:
        from echo_agent.channels.base import SendResult

        best = DeliveryResult(DeliveryStage.NO_HANDLER, channel)
        for i, result in enumerate(results):
            if isinstance(result, BaseException):
                logger.error("Outbound handler {} for channel {} failed: {}", i, channel, result)
                best = self._merge(best, DeliveryResult(DeliveryStage.FAILED, channel, error=str(result)))
            elif isinstance(result, SendResult):
                best = self._merge(best, DeliveryResult.from_send_result(result, channel))
            else:  # None
                best = self._merge(best, DeliveryResult(DeliveryStage.ACCEPTED, channel))
        return best

    @staticmethod
    def _merge(a: DeliveryResult, b: DeliveryResult) -> DeliveryResult:
        # Precedence: FAILED wins (surface any explicit failure), then DELIVERED,
        # then ACCEPTED, then NO_HANDLER.
        order = {
            DeliveryStage.FAILED: 3,
            DeliveryStage.DELIVERED: 2,
            DeliveryStage.ACCEPTED: 1,
            DeliveryStage.NO_HANDLER: 0,
        }
        return a if order[a.stage] >= order[b.stage] else b

    def subscribe_inbound(self, handler: InboundHandler) -> None:
        self._inbound_subscribers.append(handler)

    def unsubscribe_inbound(self, handler: InboundHandler) -> None:
        try:
            self._inbound_subscribers.remove(handler)
        except ValueError:
            # Unsubscribe is intentionally idempotent; absence already means the
            # handler cannot receive another inbound delivery.
            pass

    def subscribe_inbound_rejected(self, handler: InboundRejectionHandler) -> None:
        """Observe events the bus accepted but will not dispatch to handlers."""
        if handler not in self._inbound_rejection_handlers:
            self._inbound_rejection_handlers.append(handler)

    def unsubscribe_inbound_rejected(self, handler: InboundRejectionHandler) -> None:
        try:
            self._inbound_rejection_handlers.remove(handler)
        except ValueError:
            # Rejection-handler removal is idempotent for independent teardown
            # paths that may both release the same subscription.
            pass

    def subscribe_outbound(self, channel: str, handler: OutboundHandler) -> None:
        self._outbound_handlers[channel].append(handler)

    def unsubscribe_outbound(self, channel: str, handler: OutboundHandler) -> None:
        handlers = self._outbound_handlers.get(channel)
        if handlers:
            try:
                handlers.remove(handler)
            except ValueError:
                # The requested outbound subscription is already absent, which
                # is the desired result of unsubscribe.
                pass
            if not handlers:
                del self._outbound_handlers[channel]

    def subscribe_outbound_global(self, handler: OutboundHandler) -> None:
        self._global_outbound_handlers.append(handler)

    def unsubscribe_outbound_global(self, handler: OutboundHandler) -> None:
        try:
            self._global_outbound_handlers.remove(handler)
        except ValueError:
            # Global unsubscribe is idempotent across repeated lifecycle cleanup.
            pass

    async def start(self) -> None:
        async with self._lifecycle_lock:
            if self._running:
                return
            self._running = True
            async with self._admission_lock:
                self._accepting = True
            self._dispatch_task = asyncio.create_task(self._dispatch_loop())
        logger.info("MessageBus started")

    async def stop(self, drain_timeout: float = 10.0) -> None:
        async with self._lifecycle_lock:
            async with self._admission_lock:
                self._accepting = False
            # Publishers admitted before the barrier own a place in this drain.
            # Wait for their queue.put/rejection lifecycle before cancelling the
            # dispatcher and taking the final queue snapshot. Without this, a
            # delayed put could land after stop returned and truthfully report
            # neither rejection nor delivery.
            await self._publishers_idle.wait()
            self._running = False
            if self._dispatch_task:
                self._dispatch_task.cancel()
                try:
                    await self._dispatch_task
                except asyncio.CancelledError:
                    # stop() issued this cancellation and only awaits it here to
                    # ensure the dispatcher no longer owns a queued event.
                    pass
                self._dispatch_task = None
            # The dispatcher may not have observed every item accepted just
            # before shutdown. Hand the remaining bounded queue to tracked tasks
            # while subscribers (especially AgentLoop) are still alive; dropping
            # them here used to strand Gateway's already-written `accepted`
            # ledger rows forever.
            while not self._inbound_queue.empty():
                try:
                    event = self._inbound_queue.get_nowait()
                except asyncio.QueueEmpty:  # pragma: no cover - single-loop race guard
                    break
                self._schedule_inbound(event)
            # Give in-flight turns a chance to finish (and persist their
            # sessions) before hard-cancelling them.
            if self._inflight_inbound:
                tasks = list(self._inflight_inbound)
                done, pending = await asyncio.wait(tasks, timeout=drain_timeout)
                if pending:
                    logger.warning(
                        "{} in-flight event(s) did not finish within {}s, cancelling",
                        len(pending), drain_timeout,
                    )
                    rejected = [
                        self._inflight_events[task]
                        for task in pending
                        if task in self._inflight_events
                    ]
                    for task in pending:
                        task.cancel()
                    await asyncio.gather(*pending, return_exceptions=True)
                    if rejected:
                        await asyncio.gather(
                            *(self._notify_inbound_rejected(event, "shutdown") for event in rejected),
                            return_exceptions=True,
                        )
                self._inflight_inbound.clear()
                self._inflight_events.clear()
            if not self._inbound_queue.empty():
                dropped = self._inbound_queue.qsize()
                logger.warning("Discarding {} queued inbound event(s) on shutdown", dropped)
                rejected = []
                while not self._inbound_queue.empty():
                    try:
                        rejected.append(self._inbound_queue.get_nowait())
                    except asyncio.QueueEmpty:  # pragma: no cover - single-loop race guard
                        break
                await asyncio.gather(
                    *(self._notify_inbound_rejected(event, "shutdown") for event in rejected),
                    return_exceptions=True,
                )
        logger.info("MessageBus stopped")

    async def _dispatch_loop(self) -> None:
        while self._running:
            try:
                event = await asyncio.wait_for(self._inbound_queue.get(), timeout=1.0)
            except asyncio.TimeoutError:
                continue
            except asyncio.CancelledError:
                break

            # Internal control commands (e.g. clarify-cancel synthesized on ws
            # disconnect) MUST bypass the session rate limiter: they exist to
            # wake a parked turn, and a user who just flooded the session is
            # exactly the case where the escape valve is needed most. Gated on
            # the trusted typed field, never a forgeable metadata key.
            # Control commands also bypass the concurrency semaphore entirely.
            # They exist to wake or stop a parked/running turn, and those turns
            # are exactly what occupies the concurrency slots — a Ctrl+C
            # interrupt or clarify-cancel that had to queue behind full slots
            # could never reach the turn holding them (a hard deadlock when every
            # slot is a clarify wait). Control handlers return early before the
            # session lock, so they're cheap and safe to dispatch unbounded.
            self._schedule_inbound(event)

    def _schedule_inbound(self, event: InboundEvent) -> None:
        """Transfer one dequeued event into lifecycle-tracked work."""
        if event.is_control:
            task = asyncio.create_task(self._dispatch_inbound_event(event))
            self._track_inbound(task, event)
            return

        if self._rate_limiter and not self._rate_limiter.try_acquire(event.session_key):
            logger.warning("Rate limited session {}", event.session_key)
            rate_limit_reply = OutboundEvent.text_reply(
                channel=event.channel,
                chat_id=event.chat_id,
                text="请求过于频繁，请稍后再试。",
                reply_to_id=event.reply_to_id,
                is_final=True,
                message_kind="final",
            )
            rate_limit_reply.metadata = dict(event.metadata)
            rate_limit_reply.metadata["_inbound_event_id"] = event.event_id
            rate_limit_reply.metadata["_error"] = True
            rate_limit_reply.metadata["_error_reason"] = "rate limited"
            rate_limit_reply.metadata["_http_status"] = 429
            # Send the reply off-loop: a slow outbound handler must not stall
            # inbound dispatch for every other session.
            self._track_inbound(asyncio.create_task(
                self._reject_and_publish(event, "rate limited", rate_limit_reply),
            ), event)
            return

        # Acquire inside the tracked task. The old dispatcher awaited the
        # semaphore itself, leaving one dequeued event in an untracked local
        # variable that vanished when stop() cancelled the dispatcher.
        self._track_inbound(
            asyncio.create_task(self._dispatch_inbound_guarded(event)), event,
        )

    def _track_inbound(self, task: asyncio.Task, event: InboundEvent | None = None) -> None:
        self._inflight_inbound.add(task)
        if event is not None:
            self._inflight_events[task] = event

        def _done(done: asyncio.Task) -> None:
            self._inflight_inbound.discard(done)
            self._inflight_events.pop(done, None)

        task.add_done_callback(_done)

    async def _reject_and_publish(
        self, event: InboundEvent, reason: str, reply: OutboundEvent,
    ) -> None:
        await self._notify_inbound_rejected(event, reason)
        await self._publish_outbound_logged(reply)

    async def _notify_inbound_rejected(self, event: InboundEvent, reason: str) -> None:
        handlers = list(self._inbound_rejection_handlers)
        results = await asyncio.gather(
            *(handler(event, reason) for handler in handlers),
            return_exceptions=True,
        )
        for result in results:
            if isinstance(result, BaseException):
                logger.error(
                    "Inbound rejection handler failed for event {}: {}",
                    event.event_id, result,
                )

    async def _dispatch_inbound_guarded(self, event: InboundEvent) -> None:
        await self._concurrency_sem.acquire()
        try:
            await self._dispatch_inbound_event(event)
        finally:
            self._concurrency_sem.release()

    async def _publish_outbound_logged(self, event: OutboundEvent) -> None:
        try:
            await self.publish_outbound(event)
        except Exception as e:
            logger.error("Failed to publish outbound event for {}: {}", event.channel, e)

    async def _dispatch_inbound_event(self, event: InboundEvent) -> None:
        # Snapshot subscribers list to be safe against concurrent (un)subscriptions.
        subscribers = list(self._inbound_subscribers)
        results = await asyncio.gather(
            *(handler(event) for handler in subscribers),
            return_exceptions=True,
        )
        for result in results:
            if isinstance(result, Exception):
                logger.error("Inbound handler failed for event {}: {}", event.event_id, result)

    @property
    def pending_inbound(self) -> int:
        return self._inbound_queue.qsize()
