#!/usr/bin/env python3
"""Plan production bind mounts from stationd.toml using the image's tomllib."""
import json
import posixpath
import sys
import tomllib


def plan(config, directory, fallback_media):
    root = "/var/lib/stationd"
    settings = (
        ("media", "library_path", fallback_media, False),
        ("playlist", "path", "./playlist", False),
        ("grid", "path", "./grid", False),
        ("database", "path", "./data/stationd.db", True),
    )
    for section, key, default, file_path in settings:
        value = config.get(section, {}).get(key, default)
        if not isinstance(value, str) or not value or any(c in value for c in "\r\n\t\0"):
            raise ValueError(f"[{section}] {key}: invalid path")
        target = posixpath.normpath(value if value.startswith("/") else posixpath.join(root, value))
        target = "/" + target.lstrip("/")
        if not value.startswith("/") and not (target == root or target.startswith(root + "/")):
            raise ValueError(f"[{section}] {key}: relative path escapes the station directory")
        if file_path:
            target = posixpath.dirname(target)
        local = target == root or target.startswith(root + "/")
        source = posixpath.normpath(posixpath.join(directory, posixpath.relpath(target, root))) if local else target
        system_dirs = ("/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/proc", "/sys", "/dev", "/run")
        if not local and (target in ("/", "/var", "/var/lib") or any(target == p or target.startswith(p + "/") for p in system_dirs)):
            raise ValueError(f"[{section}] {key}: unsafe external directory")
        mount = json.dumps({"type": "bind", "source": source, "target": target,
                            "bind": {"create_host_path": False}}, ensure_ascii=False).replace("$", "$$")
        yield section, source, target, "local" if local else "external", mount


if __name__ == "__main__":
    try:
        with open("/stationd-config.toml", "rb") as stream:
            config = tomllib.load(stream)
        for row in plan(config, sys.argv[1], sys.argv[2]):
            print("\t".join(row))
    except (OSError, ValueError, TypeError, AttributeError) as error:
        sys.exit(f"stationd paths: {error}")
