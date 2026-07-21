from echo_agent.bus.delivery import DeliveryStage, DeliveryResult
from echo_agent.channels.base import SendResult


def test_ok_true_for_delivered_and_accepted():
    assert DeliveryResult(DeliveryStage.DELIVERED, "cli").ok is True
    assert DeliveryResult(DeliveryStage.ACCEPTED, "cli").ok is True


def test_ok_false_for_failed_and_no_handler():
    assert DeliveryResult(DeliveryStage.FAILED, "cli", error="x").ok is False
    assert DeliveryResult(DeliveryStage.NO_HANDLER, "cli").ok is False


def test_from_send_result_maps_success_and_failure():
    ok = DeliveryResult.from_send_result(SendResult(success=True, message_id="m1"), "cli")
    assert ok.stage is DeliveryStage.DELIVERED
    bad = DeliveryResult.from_send_result(SendResult(success=False, error="boom"), "cli")
    assert bad.stage is DeliveryStage.FAILED
    assert bad.error == "boom"
