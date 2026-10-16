#!/usr/bin/env python3
"""Merge the V1 fragment into an existing Traefik YAML without replacing routes.
Requires PyYAML on the operator host. Writes a candidate; deployment is separate.
"""
import argparse
import copy
from pathlib import Path
import yaml


def merge(existing, fragment):
    result = copy.deepcopy(existing)
    http = result.setdefault("http", {})
    for section in ("routers", "services", "middlewares"):
        target = http.setdefault(section, {})
        for name, value in fragment["http"].get(section, {}).items():
            if not name.startswith("stationd-v1-icecast"):
                raise ValueError("unexpected fragment key")
            if name in target and target[name] != value:
                raise ValueError("existing V1 configuration differs; review before replacing")
            target[name] = copy.deepcopy(value)
    # Every existing value remains semantically identical, including unrelated sections.
    for section in ("routers", "services", "middlewares"):
        for key, value in existing.get("http", {}).get(section, {}).items():
            assert result["http"][section][key] == value
    return result


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("existing", type=Path)
    p.add_argument("fragment", type=Path)
    p.add_argument("candidate", type=Path)
    a = p.parse_args()
    merged = merge(yaml.safe_load(a.existing.read_text()), yaml.safe_load(a.fragment.read_text()))
    a.candidate.write_text(yaml.safe_dump(merged, sort_keys=False, allow_unicode=True))
    print("Candidate written; existing routes preserved")
