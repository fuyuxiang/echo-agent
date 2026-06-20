from echo_agent.config.metadata import iter_fields
from echo_agent.config.schema import Config


def test_no_dead_fields_remain():
    dead = [
        f.snake_path for f in iter_fields(Config)
        if f.extra.get("status") == "dead"
    ]
    assert dead == [], f"still dead: {dead}"
