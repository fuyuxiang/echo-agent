"""platform 折叠 + 乐观流式通道白名单。

客户端自报的 platform 会拼进 channel="gateway:{platform}",而通道名在别处承载
能力判定(channels.stream_optimistic_channels 断言"该通道能就地重绘")。两件事
必须一起测:折叠让未知取值拿不到首方客户端的能力,白名单让 gateway:desktop 的
乐观流式有据可依。
"""

from unittest.mock import MagicMock

from echo_agent.config.schema import ChannelsConfig, GatewayConfig, GatewayPlatformConfig
from echo_agent.gateway.ws_session import normalize_platform


class TestNormalizePlatform:
    """未知取值折叠为 ws,而不是原样进入通道名。"""

    KNOWN = ["cli", "desktop", "ws", "api"]

    def test_known_platforms_pass_through(self):
        for name in self.KNOWN:
            assert normalize_platform(name, self.KNOWN) == name

    def test_unknown_platform_folds_to_ws(self):
        # 关键用例:只发客户端自报 desktop 拿不到乐观流式的能力。
        assert normalize_platform("desktop-ish", self.KNOWN) == "ws"
        assert normalize_platform("my-script", self.KNOWN) == "ws"

    def test_missing_platform_keeps_legacy_ws_default(self):
        # 旧行为:WS 握手不带 platform 时按 ws 处理。
        assert normalize_platform(None, self.KNOWN) == "ws"
        assert normalize_platform("", self.KNOWN) == "ws"
        assert normalize_platform("   ", self.KNOWN) == "ws"

    def test_surrounding_whitespace_does_not_defeat_the_fold(self):
        assert normalize_platform(" desktop ", self.KNOWN) == "desktop"

    def test_empty_known_list_disables_folding(self):
        # 显式留空 = 回到完全自报的旧行为,给需要的部署留退路。
        assert normalize_platform("anything", []) == "anything"
        assert normalize_platform("anything", None) == "anything"

    def test_fold_is_not_an_identity_control(self):
        # 折叠只收敛"通道叫什么",不做身份判定 —— 两个客户端都能自报 desktop。
        # 冒充由 resolve_client_session_key 拦,见 test_gateway_ws_session。
        assert normalize_platform("desktop", self.KNOWN) == "desktop"


class TestServerFold:
    """GatewayServer._normalize_platform 额外认可 gateway.platforms 里配过的平台。"""

    @staticmethod
    def _fold(reported, **overrides):
        from echo_agent.gateway.server import GatewayServer

        server = MagicMock()
        server._config = GatewayConfig(**overrides)
        return GatewayServer._normalize_platform(server, reported)

    def test_builtin_defaults_recognise_first_party_clients(self):
        assert self._fold("desktop") == "desktop"
        assert self._fold("cli") == "cli"

    def test_unknown_folds_under_default_config(self):
        assert self._fold("rogue") == "ws"

    def test_configured_platform_is_honoured(self):
        # 部署方为自有平台配了限流,路由必须仍走该名字而不是塌成 ws。
        folded = self._fold(
            "acme-im", platforms={"acme-im": GatewayPlatformConfig(rate_limit_rpm=10)},
        )
        assert folded == "acme-im"

    def test_missing_config_key_does_not_crash(self):
        # 老配置/构造到一半的 stub 读不到 known_platforms 时不折叠,不能抛异常。
        server = MagicMock()
        server._config = MagicMock(spec=[])
        from echo_agent.gateway.server import GatewayServer
        assert GatewayServer._normalize_platform(server, "whatever") == "whatever"


class TestOptimisticStreamDefaults:
    """默认值只放能就地重绘的通道。"""

    @staticmethod
    def _can_retract(channel: str, **overrides) -> bool:
        from echo_agent.agent.pipeline.inference_stage import InferenceStage

        stage = MagicMock()
        stage._config.channels = ChannelsConfig(**overrides)
        return InferenceStage._can_retract_draft(stage, channel)

    def test_desktop_can_retract_by_default(self):
        # Electron 端按累计缓冲重建气泡并处理 _stream_reset(见 echo-agent-desktop
        # 的 pages/Chat/index.tsx、stores/chatStore.ts)。该契约在本仓库无法验证,
        # 依据是 desktop 属于 gateway.known_platforms 里的首方客户端。
        assert self._can_retract("gateway:desktop") is True

    def test_tui_can_retract_and_plain_cli_cannot(self):
        assert self._can_retract("gateway:cli") is True
        # 纯 cli 直写 stdout,撤回会把草稿留在答案上方。
        assert self._can_retract("cli") is False

    def test_send_only_and_im_channels_stay_buffered(self):
        for channel in ("telegram", "discord", "slack", "webhook", "email"):
            assert self._can_retract(channel) is False

    def test_folded_unknown_client_lands_on_buffered_channel(self):
        # 折叠的落点必须是保守通道 —— 否则折叠反而成了提权。
        assert self._can_retract("gateway:ws") is False
        assert self._can_retract("gateway:api") is False

    def test_empty_list_disables_optimistic_streaming_everywhere(self):
        assert self._can_retract("gateway:desktop", stream_optimistic_channels=[]) is False
        assert self._can_retract("gateway:cli", stream_optimistic_channels=[]) is False
