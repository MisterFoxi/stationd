#!/usr/bin/env python3
"""Initialisation locale idempotente. Ne montre jamais les mots de passe."""
from pathlib import Path
import os
import secrets

root = Path(__file__).resolve().parents[2] / ".local/icecast/secrets"
root.mkdir(parents=True, exist_ok=True, mode=0o700)
root.chmod(0o700)
for name in ("admin-password", "admin-api-token", "source-a-password", "source-b-password", "source-radio-password", "router-api-token", "icecast-discovery-key", "icecast-admin-discovery-key", "icecast-monitor-discovery-key", "icecast-router-discovery-key"):
    path = root / name
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        if not path.read_text().strip():
            raise SystemExit(f"Secret vide : {name}")
    else:
        with os.fdopen(fd, "w") as output:
            output.write(secrets.token_hex(24) + "\n")
    # Compose bind-mounts each file, not its private parent directory.
    # Allows the unprivileged Icecast container user to read the secret.
    path.chmod(0o644)
# Only explicitly provisioned service identities/credentials enter the gateway store.
store = root.parents[1] / "stationdeck/service-credentials"
store.mkdir(parents=True, exist_ok=True, mode=0o700)
store.chmod(0o700)
for service, token_name in (("admin", "admin-api-token"), ("monitor", "admin-api-token"), ("router", "router-api-token")):
    for suffix, source in (("discovery-key", f"icecast-{service}-discovery-key"), ("token", token_name)):
        destination = store / f"icecast-{service}-1.{suffix}"
        material = (root / source).read_bytes()
        try:
            fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            if destination.read_bytes() != material:
                raise SystemExit(f"Credential mismatch; refusing replacement: {destination.name}")
        else:
            with os.fdopen(fd, "wb") as output:
                output.write(material)
                output.flush()
                os.fsync(output.fileno())
        destination.chmod(0o600)
print("Secrets locaux prêts (répertoire privé, valeurs non affichées).")


(Path(__file__).resolve().parents[2] / "data/geoip").mkdir(parents=True, exist_ok=True, mode=0o755)
