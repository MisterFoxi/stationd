#!/usr/bin/env python3
"""Add only the Webmin entries, preserving the existing config and its inode."""
import datetime
from pathlib import Path
import sys
import yaml

config = Path(sys.argv[1])
fragment = yaml.safe_load(Path(sys.argv[2]).read_text(encoding="utf-8-sig"))
original = config.read_text()
current = yaml.safe_load(original)
updated = original
for section in ("routers", "services"):
    entries = fragment["http"][section]
    existing = current["http"][section]
    missing = {}
    for name, value in entries.items():
        if name in existing:
            if existing[name] != value:
                raise SystemExit(f"Refusing to overwrite {section}.{name}")
        else:
            missing[name] = value
    if missing:
        marker = f"  {section}:\n"
        if updated.count(marker) != 1:
            raise SystemExit(f"Ambiguous {section} section")
        block = yaml.safe_dump(missing, sort_keys=False)
        block = "".join("    " + line + "\n" for line in block.splitlines())
        updated = updated.replace(marker, marker + "\n" + block, 1)
parsed = yaml.safe_load(updated)
expected = yaml.safe_load(original)
for section in ("routers", "services"):
    expected["http"][section].update(fragment["http"][section])
if parsed != expected:
    raise SystemExit("Configuration differs beyond the requested entries")
if updated != original:
    backup = config.with_name(config.name + ".before-webmin-" + datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ"))
    backup.write_text(original)
    # Keep the inode because Traefik bind-mounts this individual file.
    with config.open("w") as output:
        output.write(updated)
    print(f"Updated {config}; backup {backup}")
else:
    print("Webmin entries already present")
