"""Platform name folding and optimistic-stream channel allowlisting."""

from unittest.mock import MagicMock

from echo_agent.config.schema import ChannelsConfig, GatewayConfig, GatewayPlatformConfig
from echo_agent.gateway.ws_session import normalize_platform


class TestNormalizePlatform:
    """未知取值折叠为 ws,而不是原样进入通道名。"""

    KNOWN = ["cli", "ws", "api"]

    def test_known_platforms_pass_through(self):
        for name in self.KNOWN:
            assert normalize_platform(name, self.KNOWN) == name

    def test_unknown_platform_folds_to_ws(self):
        assert normalize_platform("legacy-ui", self.KNOWN) == "ws"
        assert normalize_platform("my-script", self.KNOWN) == "ws"

    def test_missing_platform_keeps_legacy_ws_default(self):
        # 旧行为:WS 握手不带 platform 时按 ws 处理。
        assert normalize_platform(None, self.KNOWN) == "ws"
        assert normalize_platform("", self.KNOWN) == "ws"
        assert normalize_platform("   ", self.KNOWN) == "ws"

    def test_surrounding_whitespace_does_not_defeat_the_fold(self):
        assert normalize_platform(" cli ", self.KNOWN) == "cli"

    def test_empty_known_list_disables_folding(self):
        # 显式留空 = 回到完全自报的旧行为,给需要的部署留退路。
        assert normalize_platform("anything", []) == "anything"
        assert normalize_platform("anything", None) == "anything"

    def test_fold_is_not_an_identity_control(self):
        # 折叠只收敛"通道叫什么",不做身份判定。
        # 冒充由 resolve_client_session_key 拦,见 test_gateway_ws_session。
        assert normalize_platform("cli", self.KNOWN) == "cli"


class TestServerFold:
    """GatewayServer._normalize_platform 额外认可 gateway.platforms 里配过的平台。"""

    @staticmethod
    def _fold(reported, **overrides):
        from echo_agent.gateway.server import GatewayServer

        server = MagicMock()
        server._config = GatewayConfig(**overrides)
        return GatewayServer._normalize_platform(server, reported)

    def test_builtin_defaults_recognise_attached_cli(self):
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


class TestEveryRealChannelIsKnown:
    """真实通道必须全在 known_platforms 里 —— 漏一个就切断该通道的已有授权。

    折叠发生在授权校验之前,而两套授权数据都按 platform 存:allowlist 用文档推荐的
    "feishu:123" 形式,配对用户存在 {platform}_approved.json。所以漏掉一个真实通道
    不只是路由跑偏,而是让磁盘上已有的批准数据静默失效。
    """

    def test_no_implemented_channel_folds_to_ws(self):
        # 直接读注册表,而不是手抄一份名单 —— 手抄的名单正是本次漏掉 8 个通道的原因。
        from echo_agent.channels.manager import _CHANNEL_REGISTRY

        known = set(GatewayConfig().known_platforms)
        missing = sorted(name for name in _CHANNEL_REGISTRY if name not in known)
        assert not missing, (
            f"这些通道会被折叠为 ws,升级后其已配对用户会被 403 拒绝: {missing}。"
            f"新增通道时必须同步 GatewayConfig.known_platforms"
        )

    def test_folding_preserves_platform_scoped_authorization(self):
        # 回归护栏:折叠后的取值必须还能命中 allowlist 里的 "<platform>:<user>"。
        from echo_agent.config.schema import GatewayAuthConfig
        from echo_agent.gateway.ws_session import normalize_platform

        known = GatewayConfig().known_platforms
        for platform in ("feishu", "dingtalk", "wecom", "whatsapp", "matrix", "email"):
            folded = normalize_platform(platform, known)
            assert folded == platform, f"{platform} 被折叠成了 {folded}"
            cfg = GatewayAuthConfig(mode="allowlist", allowed_users=[f"{platform}:u1"])
            assert f"{folded}:u1" in set(cfg.allowed_users)


class TestOptimisticStreamDefaults:
    """默认值只放能就地重绘的通道。"""

    @staticmethod
    def _can_retract(channel: str, **overrides) -> bool:
        from echo_agent.agent.pipeline.inference_stage import InferenceStage

        stage = MagicMock()
        stage._config.channels = ChannelsConfig(**overrides)
        return InferenceStage._can_retract_draft(stage, channel)

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
        assert self._can_retract("gateway:cli", stream_optimistic_channels=[]) is False
