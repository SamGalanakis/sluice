"""Copy only provider routing from the source Codex configuration into a private fixture."""
import json
from pathlib import Path
import sys
import tomllib

path = Path(sys.argv[1])
config = tomllib.loads(path.read_text()) if path.exists() else {}
for key in ("model_provider", "service_tier"):
    if key in config:
        print(f"{key} = {json.dumps(config[key])}")
for name, values in config.get("model_providers", {}).items():
    print(f"\n[model_providers.{json.dumps(name)}]")
    for key, value in values.items():
        assert isinstance(value, (str, int, float, bool, list)), "unexpected nested provider config"
        print(f"{json.dumps(key)} = {json.dumps(value)}")
