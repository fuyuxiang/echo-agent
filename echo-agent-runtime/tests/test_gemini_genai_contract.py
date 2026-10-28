"""Real google-genai validation/serialization with the network edge isolated."""
import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

import httpx
import pytest
from google import genai
from google.genai import types

from echo_agent.models.providers.gemini_provider import GeminiProvider


@pytest.mark.asyncio
async def test_real_sdk_roundtrip_tools_signatures_inline_image_and_usage():
    requests = []
    responses = [
        {'candidates': [{'content': {'role': 'model', 'parts': [
            {'functionCall': {'id': 'same-1', 'name': 'read_file', 'args': {'path': 'a'}},
             'thoughtSignature': 'c2lnbmVk'},
            {'functionCall': {'id': 'same-2', 'name': 'read_file', 'args': {'path': 'b'}}}]},
            'finishReason': 'STOP'}],
         'usageMetadata': {'promptTokenCount': 5, 'candidatesTokenCount': 3}},
        {'candidates': [{'content': {'role': 'model', 'parts': [{'text': 'done'}]}, 'finishReason': 'STOP'}]},
    ]
    async def send(request):
        import json
        requests.append((str(request.url), json.loads(request.content)))
        return httpx.Response(200, json=responses.pop(0))
    client = genai.Client(api_key='test-key', vertexai=False, http_options={
        'base_url': 'https://example.test/v1beta',
        'async_client_args': {'transport': httpx.MockTransport(send)},
    }).aio
    with patch.object(GeminiProvider, '_build_client', return_value=client):
        provider = GeminiProvider(api_key='test-key', default_model='test-model')
    messages = [{'role': 'system', 'content': 'system'}, {'role': 'user', 'content': [
        {'type': 'text', 'text': 'read both'},
        {'type': 'image_url', 'image_url': {'url': 'data:image/png;base64,aW1n'}},
    ]}]
    tools = [{'type': 'function', 'function': {'name': 'read_file', 'parameters': {
        'type': 'object', 'properties': {'path': {'type': 'string'}}, 'required': ['path'],
    }}}]
    try:
        result = await provider.chat(messages, tools=tools, tool_choice='required')
        assert result.finish_reason == 'tool_calls', result.content
        assert [call.id for call in result.tool_calls] == ['same-1', 'same-2']
        assert result.usage['prompt_tokens'] == 5
        assert result.provider_content['gemini'][0]['thought_signature'] == 'c2lnbmVk'
        messages.append({'role': 'assistant', 'content': '', 'provider_content': result.provider_content,
                         'tool_calls': [c.to_openai_format() for c in result.tool_calls]})
        messages.extend({'role': 'tool', 'name': c.name, 'tool_call_id': c.id, 'content': 'ok'}
                        for c in result.tool_calls)
        final = await provider.chat(messages, tools=tools)
        assert final.content == 'done'
        assert requests[0][0].count('/v1beta/') == 1
        body = requests[1][1]
        assert body['contents'][1]['parts'][0]['thoughtSignature'] == 'c2lnbmVk'
        assert body['contents'][2]['parts'][1]['functionResponse']['id'] == 'same-2'
        assert body['toolConfig']['functionCallingConfig']['mode'] == 'AUTO'
        assert requests[0][1]['contents'][0]['parts'][1]['inlineData']['data'] == 'aW1n'
    finally:
        await provider.aclose()


@pytest.mark.asyncio
async def test_stream_cancel_closes_iterator_and_emits_reasoning_separately():
    entered, closed = asyncio.Event(), asyncio.Event()
    async def iterate():
        try:
            yield types.GenerateContentResponse(candidates=[types.Candidate(content=types.Content(parts=[
                types.Part(text='thinking', thought=True), types.Part(text='answer'),
            ]))])
            entered.set()
            await asyncio.Event().wait()
        finally:
            closed.set()
    client = SimpleNamespace(models=SimpleNamespace(generate_content_stream=AsyncMock(return_value=iterate())))
    with patch.object(GeminiProvider, '_build_client', return_value=client):
        p = GeminiProvider(default_model='test')
    text, reasoning = [], []
    call = asyncio.create_task(p.chat_stream([{'role': 'user', 'content': 'x'}],
                                            on_delta=text.append, on_reasoning=reasoning.append))
    await asyncio.wait_for(entered.wait(), 1)
    call.cancel()
    with pytest.raises(asyncio.CancelledError):
        await call
    assert closed.is_set()
    assert text == ['answer'] and reasoning == ['thinking']


def test_credentials_are_client_local_and_api_base_is_honored():
    with patch('google.genai.Client') as client:
        first = GeminiProvider(api_key='first', api_base='https://proxy.test', extra_headers={'X-Tenant': 'a'})
        GeminiProvider(api_key='second')
    assert client.call_args_list[0].kwargs == {'api_key': 'first', 'vertexai': False,
        'http_options': {'base_url': 'https://proxy.test', 'headers': {'X-Tenant': 'a'}}}
    assert client.call_args_list[1].kwargs['api_key'] == 'second'
    assert first.api_key == 'first'


def test_blocked_prompt_is_not_empty_success():
    with patch.object(GeminiProvider, '_build_client'):
        p = GeminiProvider()
    result = p._parse_response(types.GenerateContentResponse(
        prompt_feedback=types.GenerateContentResponsePromptFeedback(block_reason='SAFETY')), 'test')
    assert result.finish_reason == 'content_filter'
