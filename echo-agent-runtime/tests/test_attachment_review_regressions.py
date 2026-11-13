"""Negative cases found during the attachment/reminder product review."""
import importlib.util
import json
import time
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from echo_agent.agent.approval_gate import APPROVAL_SOURCE_HUMAN
from echo_agent.agent.context import ContextBuilder
from echo_agent.agent.media.document_extract import extract
from echo_agent.agent.tools.attachments import AttachmentDeleteTool
from echo_agent.agent.tools.cronjob import CronjobTool
from echo_agent.agent.tools.filesystem import ReadFileTool
from echo_agent.attachments import AttachmentStore, document_extension
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig
from echo_agent.scheduler.authorization import grant, verify
from echo_agent.scheduler.reminders import resume_reminders
from echo_agent.scheduler.service import JobStatus, ScheduledJob, Scheduler, TriggerKind

from tests.test_attachment_reminders import context, future, original
from tests import test_attachment_reminders as reminder_tests

runtime = reminder_tests.runtime


@pytest.mark.asyncio
async def test_quoted_pdf_signature_remains_text(tmp_path):
    source = tmp_path / '说明.txt'
    source.write_text('PDF 文件签名是 %PDF-1.7，不能据此误判整个文档。')
    assert document_extension(source) == '.txt'
    assert '文件签名' in extract(source).text
    result = await ReadFileTool(str(tmp_path)).execute({'path': str(source)})
    assert result.success and '%PDF-1.7' in result.output
    code = Path(__file__).parents[1] / 'echo_agent' / 'attachments.py'
    assert (await ReadFileTool(str(code.parent)).execute({'path': str(code)})).success


@pytest.mark.asyncio
@pytest.mark.parametrize('human', [False, True])
@pytest.mark.parametrize('authorized', [False, True])
async def test_rename_neither_grants_nor_revokes_existing_consent(tmp_path, human, authorized):
    scheduler = Scheduler(tmp_path / 'scheduler.json')
    job = scheduler.add_job(ScheduledJob(cron_expr='0 9 * * *', payload={
        'command': '执行备份并写文件', 'source_session_key': 'weixin:user'}))
    if authorized:
        job.authorization = grant(job, operator='user', source='test')
    previous = job.authorization
    ctx = replace(context(), approval_source=APPROVAL_SOURCE_HUMAN) if human else context()
    tool = CronjobTool(scheduler)
    args = {'action': 'update', 'job_id': job.id, 'name': '改名称'}
    assert job.payload['command'] in tool.describe_for_approval(args)
    assert (await tool.execute(args, ctx)).success
    assert verify(job) is authorized and job.authorization is previous
    assert (await tool.execute(dict(args, command='不同的写入指令'), ctx)).success
    assert not verify(job)


@pytest.mark.asyncio
async def test_distinct_reminders_in_same_message_are_not_deduplicated(runtime):
    r = runtime
    first = {'action': 'create', 'name': '提醒', 'at': future(), 'delivery_text': '交报告'}
    one = await r.tool.execute(first, context())
    two = await r.tool.execute(dict(first, delivery_text='参加会议'), context())
    retry = await r.tool.execute(first, context())
    assert one.metadata['job_id'] != two.metadata['job_id']
    assert retry.metadata['job_id'] == one.metadata['job_id']
    assert len(r.scheduler.list_jobs()) == 2


@pytest.mark.asyncio
async def test_update_validates_final_target_before_mutating(runtime):
    r = runtime
    record = original(r.tmp_path, r.store)
    r.tool._channel_lookup = lambda name: SimpleNamespace(supports_files=name == 'weixin')
    created = await r.tool.execute({'action': 'create', 'at': future(), 'delivery_text': '提醒',
        'attachment_ids': [record['id']]}, context())
    job = r.scheduler.get_job(created.metadata['job_id'])
    result = await r.tool.execute({'action': 'update', 'job_id': job.id,
        'target_channel': 'telegram', 'target_chat_id': 'other'}, context())
    assert not result.success and '不支持' in result.error
    assert job.payload['deliver_channel'] == 'weixin'
    assert (await r.tool.execute({'action': 'update', 'job_id': job.id, 'attachment_ids': [],
        'target_channel': 'telegram', 'target_chat_id': 'other'}, context())).success


def test_completed_once_can_be_rescheduled_to_recurring(tmp_path):
    scheduler = Scheduler(tmp_path / 'scheduler.json')
    job = scheduler.add_job(ScheduledJob(trigger=TriggerKind.ONCE, at_ms=1, status=JobStatus.COMPLETED))
    scheduler.update_job(job.id, trigger=TriggerKind.CRON, cron_expr='0 9 * * *', at_ms=0, delete_after_run=False)
    assert job.status == JobStatus.ACTIVE and job.next_run_ms > time.time() * 1000


@pytest.mark.asyncio
async def test_caption_prevents_silent_binding_and_is_processed_by_model(tmp_path):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.config.loader import load_config
    from echo_agent.models.provider import LLMResponse
    spec = importlib.util.spec_from_file_location('loop_helpers', Path(__file__).with_name('test_agent_loop_core.py'))
    helpers = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helpers)
    provider = helpers._StubProvider([LLMResponse(content='已单独保存，不加入甲的提醒。', finish_reason='stop')])
    scheduler = Scheduler(tmp_path / 'scheduler.json')
    store = AttachmentStore(tmp_path)
    created = await CronjobTool(scheduler, store).execute({'action': 'create', 'at': future(),
        'delivery_text': '甲的面试', 'await_attachment': True}, context())
    agent = AgentLoop(bus=MessageBus(), config=load_config(overrides={
        'workspace': str(tmp_path), 'memory': {'enabled': False}}), provider=provider,
        workspace=tmp_path, scheduler=scheduler)
    file = tmp_path / '乙.txt'
    file.write_text('乙候选人')
    event = InboundEvent(channel='weixin', sender_id='user', chat_id='user', content=[
        ContentBlock(type=ContentType.TEXT, text='这是乙的简历，只保存，不要关联甲的提醒。'),
        ContentBlock(type=ContentType.FILE, url=str(file), metadata={'name': file.name})])
    result = await agent._process_event(event, 'caption-regression')
    assert '单独保存' in result.response_text
    job = scheduler.get_job(created.metadata['job_id'])
    assert not job.payload.get('attachment_ids') and job.payload['await_attachment']
    assert (await CronjobTool(scheduler, store).execute({'action': 'update', 'job_id': job.id,
        'name': '甲的面试改名'}, context())).success
    assert not job.payload.get('attachment_ids')


@pytest.mark.parametrize('channel', ['telegram', 'slack', 'feishu', 'matrix', 'weixin'])
@pytest.mark.asyncio
async def test_adapters_cache_inside_custom_workspace(tmp_path, channel):
    cache = tmp_path / 'custom' / 'data' / 'media_cache'
    config = ChannelsConfig()
    config.weixin.data_dir = str(tmp_path / 'weixin')
    manager = ChannelManager(config, MessageBus(), media_cache_root=cache)
    adapter = manager._build_channel(channel)
    path = await adapter._resolve_media_to_cache('test-cache', channel, AsyncMock(return_value=b'original'), suffix='.txt')
    assert Path(path).is_relative_to(cache)
    store = AttachmentStore(tmp_path / 'custom')
    saved = store.save(channel + ':user', 'test', 0, {'url': path, 'type': 'file', 'name': '简历.txt'})
    assert store.path(saved).read_bytes() == b'original'


@pytest.mark.asyncio
async def test_trusted_external_cache_is_allowed_but_arbitrary_paths_are_not(tmp_path):
    workspace = tmp_path / 'workspace'
    workspace.mkdir()
    source = tmp_path / 'custom-cache' / 'file.txt'
    source.parent.mkdir()
    source.write_text('original')
    builder = ContextBuilder(workspace)
    builder.attachments.trusted_roots.add(source.parent)
    event = InboundEvent(channel='slack', sender_id='u', chat_id='u', content=[
        ContentBlock(type=ContentType.FILE, url=str(source))])
    items = await builder.prepare_event_media(event)
    assert items[0]['attachment_id']
    other = tmp_path / 'untrusted.txt'
    other.write_text('secret')
    with pytest.raises(ValueError, match='workspace'):
        builder.attachments.save('slack:u', 'forged', 0, {'url': str(other)})


@pytest.mark.asyncio
@pytest.mark.parametrize('edit', ['pause', 'payload', 'delete'])
async def test_pending_outbox_does_not_recover_cancelled_intent(runtime, edit):
    r = runtime
    record = original(r.tmp_path, r.store)
    result = await r.tool.execute({'action': 'create', 'at': future(), 'delivery_text': '提醒',
        'attachment_ids': [record['id']]}, context())
    job = r.scheduler.get_job(result.metadata['job_id'])
    r.channel._send_file.return_value = SendResult(False, error='context expired', recovery_action='send_message')
    await r.scheduler.trigger_job(job.id)
    assert job.last_status == 'delivery_pending'
    if edit == 'pause':
        r.scheduler.update_job(job.id, enabled=False)
        r.scheduler.update_job(job.id, enabled=True)
    elif edit == 'payload':
        r.scheduler.update_job(job.id, payload=dict(job.payload, delivery_text='新内容'))
    else:
        r.scheduler.remove_job(job.id)
    await r.outputs._recover('weixin', 'user')
    assert not await r.ledger.waiting_outbound('weixin', 'user')
    assert r.channel._send_file.await_count == 1
    r.transport.assert_awaited_once()


@pytest.mark.asyncio
async def test_paused_interrupted_reminder_is_not_replayed_on_startup(runtime):
    r = runtime
    job = r.scheduler.add_job(ScheduledJob(enabled=False, status=JobStatus.PAUSED,
        payload={'delivery_mode': 'reminder', 'delivery_text': '已暂停', 'source_session_key': 'weixin:user'},
        run_history=[{'run_id': 'old', 'status': 'interrupted'}]))
    await resume_reminders(r.outputs)
    r.transport.assert_not_awaited()
    assert job.run_history[0]['status'] == 'interrupted'


def test_attachment_retention_protects_reminders_and_pending_runs(tmp_path):
    store = AttachmentStore(tmp_path)
    scheduler = Scheduler(tmp_path / 'data' / 'scheduler.json')
    pinned = original(tmp_path, store, event='pinned')
    pending = original(tmp_path, store, event='pending')
    expired = original(tmp_path, store, event='expired')
    for record in [pinned, pending, expired]:
        path = store._scope('weixin:user') / record['id'] / 'manifest.json'
        record['received_at'] = time.time() - 40 * 86400
        path.write_text(json.dumps(record))
        with store._catalog() as db:
            db.execute('UPDATE originals SET received=? WHERE id=?', (record['received_at'], record['id']))
    job = scheduler.add_job(ScheduledJob(payload={'attachment_ids': [pinned['id']]}, enabled=False))
    scheduler.add_job(ScheduledJob(status=JobStatus.COMPLETED, payload={}, run_history=[{
        'run_id': 'pending', 'status': 'delivery_pending', 'reminder_payload': {'attachment_ids': [pending['id']]}}]))
    assert store.cleanup(force=True) == 1
    assert store.get('weixin:user', pinned['id']) and store.get('weixin:user', pending['id'])
    with pytest.raises(ValueError, match='引用'):
        store.delete('weixin:user', pinned['id'])
    scheduler.remove_job(job.id)
    store.delete('weixin:user', pinned['id'])
    assert not store.path(pinned).exists()
    with pytest.raises(ValueError, match='删除'):
        original(tmp_path, store, event='pinned')


def test_attachment_quota_never_evicts_recent_originals(tmp_path):
    store = AttachmentStore(tmp_path)
    store.configure(retention_days=30, max_mb=1)
    path = tmp_path / 'large.bin'
    path.write_bytes(b'x' * (700 * 1024))
    first = store.save('weixin:user', 'first', 0, {'url': str(path), 'type': 'file'})
    with pytest.raises(ValueError, match='空间已满'):
        store.save('weixin:user', 'second', 0, {'url': str(path), 'type': 'file'})
    assert store.path(store.get('weixin:user', first['id'])).is_file()
    store.delete('weixin:user', first['id'])
    assert store.save('weixin:user', 'second', 0, {'url': str(path), 'type': 'file'})


def test_legacy_catalog_is_imported_once_and_recent_uses_index(tmp_path, monkeypatch):
    store = AttachmentStore(tmp_path)
    saved = original(tmp_path, store)
    (store.root / 'catalog.sqlite3').unlink()
    assert AttachmentStore(tmp_path).recent('weixin:user')[0]['id'] == saved['id']
    def no_full_scan(*args, **kwargs):
        raise AssertionError('Indexed listing must not scan manifests')
    monkeypatch.setattr(Path, 'glob', no_full_scan)
    assert store.recent('weixin:user')[0]['id'] == saved['id']


@pytest.mark.asyncio
async def test_attachment_delete_is_session_scoped(tmp_path):
    saved = original(tmp_path, AttachmentStore(tmp_path))
    tool = AttachmentDeleteTool(tmp_path)
    assert not (await tool.execute({'attachment_id': saved['id']}, context('weixin:other'))).success
    assert (await tool.execute({'attachment_id': saved['id']}, context())).success


@pytest.mark.asyncio
async def test_rest_attachment_names_and_target_capability(runtime):
    from echo_agent.gateway.api.cron_api import CronAPI
    r = runtime
    saved = original(r.tmp_path, r.store)
    server = SimpleNamespace(_require_admin_token=lambda *a, **kw: None,
        _agent_loop=SimpleNamespace(scheduler=r.scheduler), _workspace=r.tmp_path,
        channel_manager=SimpleNamespace(get_channel=lambda name: SimpleNamespace(supports_files=name == 'weixin')))
    api = CronAPI(server)
    payload = {'command': '提醒', 'delivery_text': '提醒', 'delivery_mode': 'reminder',
        'source_session_key': 'weixin:user', 'attachment_ids': [saved['id']],
        'deliver_channel': 'weixin', 'deliver_chat_id': 'user'}
    request = SimpleNamespace(json=AsyncMock(return_value={'trigger': 'once', 'at': future(), 'payload': payload}))
    response = await api.create_job(request)
    assert response.status == 201
    job = r.scheduler.get_job(json.loads(response.text)['id'])
    assert api._job_to_dict(job)['attachment_details'] == [{'id': saved['id'], 'name': saved['name'], 'available': True}]
    request = SimpleNamespace(match_info={'id': job.id}, json=AsyncMock(return_value={
        'payload': {'deliver_channel': 'unsupported', 'deliver_chat_id': 'other'}}))
    response = await api.update_job(request)
    assert response.status == 400
    assert job.payload['deliver_channel'] == 'weixin'


@pytest.mark.asyncio
async def test_expired_binding_receipt_is_truthful_and_explicit_wait_renews(runtime):
    r = runtime
    result = await r.tool.execute({'action': 'create', 'at': future(), 'delivery_text': '提醒',
        'await_attachment': True}, context())
    job = r.scheduler.get_job(result.metadata['job_id'])
    job.payload['attachment_window_end'] = time.time() - 60
    assert '窗口已结束' in r.tool._receipt(job)
    renamed = await r.tool.execute({'action': 'update', 'job_id': job.id, 'name': '新名称'}, context())
    assert renamed.success and job.payload['attachment_window_end'] < time.time()
    renewed = await r.tool.execute({'action': 'update', 'job_id': job.id, 'await_attachment': True}, context())
    assert renewed.success and job.payload['attachment_window_end'] > time.time() + 590


@pytest.mark.asyncio
async def test_dispatch_error_before_first_intent_recovers_original_run(runtime):
    r = runtime
    result = await r.tool.execute({'action': 'create', 'at': future(), 'delivery_text': '持久提醒'}, context())
    job = r.scheduler.get_job(result.metadata['job_id'])
    prepare = r.outputs.prepare
    r.outputs.prepare = AsyncMock(side_effect=OSError('temporary disk failure'))
    await r.scheduler.trigger_job(job.id)
    assert job.last_status == 'error' and job.run_count == 1
    r.transport.assert_not_awaited()
    r.outputs.prepare = prepare
    await resume_reminders(r.outputs)
    assert job.last_status == 'completed' and job.run_count == 1
    r.transport.assert_awaited_once()


@pytest.mark.asyncio
async def test_bad_recovery_intent_does_not_block_other_reminders(runtime):
    r = runtime
    bad = r.scheduler.add_job(ScheduledJob(payload={'delivery_mode': 'reminder',
        'source_session_key': 'weixin:user'}, run_history=[{'run_id': 'bad', 'status': 'interrupted'}]))
    await resume_reminders(r.outputs)
    assert bad.last_status == 'error'
    result = await r.tool.execute({'action': 'create', 'at': future(), 'delivery_text': '正常提醒'}, context())
    assert await r.scheduler.trigger_job(result.metadata['job_id'])
    r.transport.assert_awaited_once()
