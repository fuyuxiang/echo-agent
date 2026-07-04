from echo_agent.config.schema import Config


def test_checkpoint_config_defaults():
    cfg = Config()
    assert cfg.checkpoint.enabled is True
    assert cfg.checkpoint.max_snapshots_per_workspace == 20
    assert cfg.checkpoint.max_total_size_mb == 500
    assert cfg.checkpoint.max_file_size_mb == 10
    assert cfg.checkpoint.store_path.endswith("checkpoints/store")


def test_checkpoint_config_fields_have_bilingual_desc():
    fields = Config().checkpoint.model_fields
    for name, f in fields.items():
        extra = f.json_schema_extra or {}
        assert extra.get("desc_zh"), f"{name} missing desc_zh"
        assert extra.get("desc_en"), f"{name} missing desc_en"
