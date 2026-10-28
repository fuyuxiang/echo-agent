"""weixin 发送路径的失败判定回归测试。

背景:一次"定时任务日志全绿但微信没收到"的事故。轮询循环一直同时检查
``errcode`` 与 ``ret``,发送路径却只检查 ``errcode`` 且缺省为 0,于是形如
``{"ret": -14}``(会话过期)的响应在轮询侧是致命错误,在发送侧被读成发送成功。
``_api_post`` 也从不检查 HTTP 状态码,非 2xx 时网关错误页解析出来没有 errcode,
同样被读成成功。

这里锁住两件事:两条路径共用同一套失败判定、非 2xx 不再被当成功。
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from echo_agent.channels import weixin as wx
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.config.schema import WeixinChannelConfig
from echo_agent.bus.queue import MessageBus


def _make_weixin(tmp_path: Path) -> WeixinChannel:
    cfg = WeixinChannelConfig(
        account_id="acct@im.bot",
        token="acct@im.bot:tok",
        data_dir=str(tmp_path / "weixin"),
    )
    ch = WeixinChannel(cfg, MessageBus())
    ch._send_session = MagicMock()  # truthy; real calls are monkeypatched
    return ch


class _FakeResponse:
    """最小的 aiohttp 响应替身,只实现 _api_post 用到的接口。"""

    def __init__(self, status: int = 200, json_data=None, text_data: str = ""):
        self.status = status
        self._json = json_data
        self._text = text_data

    async def json(self, content_type=None):
        return self._json

    async def text(self) -> str:
        return self._text

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc_info):
        return False


class _FakeSession:
    def __init__(self, response: _FakeResponse):
        self._response = response
        self.posts: list[str] = []

    def post(self, url, **kwargs):
        self.posts.append(url)
        return self._response


# ── 1. 失败判定本身 ───────────────────────────────────────────────────────────

class TestApiError:
    @pytest.mark.parametrize("field", ["errcode", "ret"])
    def test_nonzero_code_in_either_field_is_a_failure(self, field):
        assert _api_error_of({field: 99, "errmsg": "boom"})
        assert "99" in _api_error_of({field: 99, "errmsg": "boom"})

    @pytest.mark.parametrize("field", ["errcode", "ret"])
    def test_session_expiry_is_named_in_either_field(self, field):
        msg = _api_error_of({field: wx._SESSION_EXPIRED_ERRCODE})
        assert "session expired" in msg
        assert "re-login" in msg

    @pytest.mark.parametrize(
        "response",
        [
            {},
            {"errcode": 0},
            {"ret": 0},
            {"errcode": 0, "ret": 0},
            {"errcode": None, "ret": None},
            {"msgs": []},
        ],
    )
    def test_absent_or_zero_codes_are_success(self, response):
        # 两个字段在正常响应里本就可以不出现,把缺失当失败会把每次正常发送判成错。
        assert _api_error_of(response) == ""

    def test_non_numeric_code_is_a_failure_not_a_crash(self):
        assert _api_error_of({"errcode": "bad"})

    @pytest.mark.parametrize("response", [{}, None, [], ""])
    def test_require_ok_rejects_empty_or_non_dict_bodies(self, response):
        failure = wx._require_ok(response, what="sendmessage")
        assert "empty or non-JSON" in failure

    def test_require_ok_passes_a_normal_success_body(self):
        assert wx._require_ok({"errcode": 0, "errmsg": "ok"}, what="sendmessage") == ""


def _api_error_of(response) -> str:
    return wx._api_error(response)


# ── 2. 文本发送 ───────────────────────────────────────────────────────────────

@pytest.fixture
def _api_post_returning(monkeypatch):
    """把 _api_post 换成返回固定响应,并记录调用次数。"""

    def _install(response):
        calls: list[dict] = []

        async def fake_api_post(session, *, base_url, endpoint, payload, token, timeout_ms):
            calls.append({"endpoint": endpoint, "payload": payload})
            return response

        monkeypatch.setattr(wx, "_api_post", fake_api_post)
        return calls

    return _install


class TestSendTextFailureDetection:
    @pytest.mark.asyncio
    async def test_ret_only_session_expiry_is_not_reported_as_success(
        self, tmp_path, _api_post_returning
    ):
        """事故的核心场景:{"ret": -14} 过去被发送路径读成成功。"""
        ch = _make_weixin(tmp_path)
        _api_post_returning({"ret": wx._SESSION_EXPIRED_ERRCODE})

        res = await ch._send_text("user@im", "北京天气播报")
        assert res.success is False
        assert "session expired" in res.error

    @pytest.mark.asyncio
    async def test_ret_only_generic_error_fails(self, tmp_path, _api_post_returning):
        ch = _make_weixin(tmp_path)
        _api_post_returning({"ret": 42, "errmsg": "quota exceeded"})

        res = await ch._send_text("user@im", "hi")
        assert res.success is False
        assert "42" in res.error
        assert "quota exceeded" in res.error

    @pytest.mark.asyncio
    async def test_errcode_error_still_fails(self, tmp_path, _api_post_returning):
        ch = _make_weixin(tmp_path)
        _api_post_returning({"errcode": 40001, "errmsg": "invalid token"})

        res = await ch._send_text("user@im", "hi")
        assert res.success is False
        assert "40001" in res.error

    @pytest.mark.asyncio
    async def test_empty_body_is_not_success(self, tmp_path, _api_post_returning):
        ch = _make_weixin(tmp_path)
        _api_post_returning({})

        res = await ch._send_text("user@im", "hi")
        assert res.success is False
        assert "empty or non-JSON" in res.error

    @pytest.mark.asyncio
    async def test_normal_success_body_still_succeeds(self, tmp_path, _api_post_returning):
        ch = _make_weixin(tmp_path)
        calls = _api_post_returning({"errcode": 0, "errmsg": "ok"})

        res = await ch._send_text("user@im", "hi")
        assert res.success is True
        assert len(calls) == 1

    @pytest.mark.asyncio
    async def test_failure_on_first_chunk_stops_sending_the_rest(
        self, tmp_path, _api_post_returning
    ):
        ch = _make_weixin(tmp_path)
        calls = _api_post_returning({"ret": 42})

        res = await ch._send_text("user@im", "x" * (wx._MAX_MESSAGE_LENGTH * 2 + 10))
        assert res.success is False
        # 分片发送在第一片失败后就返回,不继续把后续片段推给一个已失效的会话。
        assert len(calls) == 1


# ── 3. 媒体发送(事故里实际走的路径) ─────────────────────────────────────────

class TestSendFileFailureDetection:
    @pytest.fixture
    def _media_env(self, monkeypatch):
        """打通 getuploadurl → CDN 上传,让 sendmessage 的响应成为唯一变量。"""

        def _install(*, upload_response=None, send_response=None):
            upload_response = upload_response if upload_response is not None else {
                "upload_full_url": "https://cdn.example/upload?x=1"
            }
            send_response = send_response if send_response is not None else {"errcode": 0}

            async def fake_get_upload_url(session, **kwargs):
                return upload_response

            async def fake_upload_ciphertext(session, *, ciphertext, upload_url):
                return "ENCRYPTED_PARAM_TOKEN"

            async def fake_api_post(session, **kwargs):
                return send_response

            monkeypatch.setattr(wx, "_get_upload_url", fake_get_upload_url)
            monkeypatch.setattr(wx, "_upload_ciphertext", fake_upload_ciphertext)
            monkeypatch.setattr(wx, "_api_post", fake_api_post)

        return _install

    def _audio(self, tmp_path: Path) -> Path:
        f = tmp_path / "beijing_weather.mp3"
        f.write_bytes(b"\xff\xfb\x90\x00 fake mpeg frames")
        return f

    @pytest.mark.asyncio
    async def test_sendmessage_ret_expiry_is_not_reported_as_success(
        self, tmp_path, _media_env
    ):
        """报成功但没送达的那条语音播报,现在会明确失败。"""
        ch = _make_weixin(tmp_path)
        _media_env(send_response={"ret": wx._SESSION_EXPIRED_ERRCODE})

        res = await ch._send_file("user@im", str(self._audio(tmp_path)), as_image=False)
        assert res.success is False
        assert "sendmessage" in res.error
        assert "session expired" in res.error

    @pytest.mark.asyncio
    async def test_sendmessage_empty_body_is_not_success(self, tmp_path, _media_env):
        ch = _make_weixin(tmp_path)
        _media_env(send_response={})

        res = await ch._send_file("user@im", str(self._audio(tmp_path)), as_image=False)
        assert res.success is False
        assert "empty or non-JSON" in res.error

    @pytest.mark.asyncio
    async def test_getuploadurl_ret_error_fails_before_uploading(self, tmp_path, _media_env):
        ch = _make_weixin(tmp_path)
        _media_env(upload_response={"ret": 7, "errmsg": "no permission"})

        res = await ch._send_file("user@im", str(self._audio(tmp_path)), as_image=False)
        assert res.success is False
        assert "getuploadurl" in res.error
        assert "7" in res.error

    @pytest.mark.asyncio
    async def test_success_path_unchanged(self, tmp_path, _media_env):
        ch = _make_weixin(tmp_path)
        _media_env()

        res = await ch._send_file("user@im", str(self._audio(tmp_path)), as_image=False)
        assert res.success is True
        assert res.message_id  # 本地 client_id,仅作事件关联


# ── 4. HTTP 状态码 ────────────────────────────────────────────────────────────

class TestApiPostHttpStatus:
    @pytest.mark.asyncio
    @pytest.mark.parametrize("status", [401, 403, 429, 500, 502])
    async def test_non_2xx_raises_instead_of_parsing_the_body(self, status):
        # 网关错误页解析出来没有 errcode,过去被下游读成发送成功。
        session = _FakeSession(_FakeResponse(status=status, json_data={}, text_data="<html>nope</html>"))
        with pytest.raises(RuntimeError) as excinfo:
            await wx._api_post(
                session, base_url="https://api.example", endpoint=wx._EP_SEND_MESSAGE,
                payload={"msg": {}}, token="tok", timeout_ms=1000,
            )
        assert str(status) in str(excinfo.value)
        assert wx._EP_SEND_MESSAGE in str(excinfo.value)

    @pytest.mark.asyncio
    async def test_2xx_returns_the_parsed_body(self):
        session = _FakeSession(_FakeResponse(status=200, json_data={"errcode": 0}))
        out = await wx._api_post(
            session, base_url="https://api.example", endpoint=wx._EP_SEND_MESSAGE,
            payload={"msg": {}}, token="tok", timeout_ms=1000,
        )
        assert out == {"errcode": 0}

    @pytest.mark.asyncio
    async def test_http_error_surfaces_as_a_send_failure(self, tmp_path, monkeypatch):
        """端到端:非 2xx 不会变成 SendResult(success=True)。"""
        ch = _make_weixin(tmp_path)
        ch._send_session = _FakeSession(
            _FakeResponse(status=502, json_data={}, text_data="bad gateway")
        )

        res = await ch._send_text("user@im", "hi")
        assert res.success is False
        assert "502" in res.error


# ── 5. 两条路径同源 ───────────────────────────────────────────────────────────

class TestPollAndSendAgree:
    @pytest.mark.parametrize(
        "response",
        [
            {"ret": wx._SESSION_EXPIRED_ERRCODE},
            {"errcode": wx._SESSION_EXPIRED_ERRCODE},
            {"ret": 42},
            {"errcode": 40001},
        ],
    )
    @pytest.mark.asyncio
    async def test_a_response_the_poll_loop_rejects_is_never_a_send_success(
        self, tmp_path, response, monkeypatch
    ):
        """同一个响应不能出现"轮询侧致命、发送侧成功"的分歧 —— 那正是事故成因。"""
        # 轮询侧:被判为错误。
        assert wx._api_error(response) != ""

        # 发送侧:同一个响应必须也失败。
        ch = _make_weixin(tmp_path)

        async def fake_api_post(session, **kwargs):
            return response

        monkeypatch.setattr(wx, "_api_post", fake_api_post)
        res = await ch._send_text("user@im", "hi")
        assert res.success is False

    @pytest.mark.asyncio
    async def test_poll_loop_still_pauses_on_ret_only_expiry(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        ch._poll_session = MagicMock()
        ch._running = True
        sleeps: list[float] = []

        async def fake_sleep(d):
            sleeps.append(d)
            ch._running = False

        async def fake_get_updates(session, **kwargs):
            return {"ret": wx._SESSION_EXPIRED_ERRCODE}

        monkeypatch.setattr(wx, "_get_updates", fake_get_updates)
        monkeypatch.setattr(wx.asyncio, "sleep", fake_sleep)
        await ch._poll_loop()
        assert sleeps == [600]
