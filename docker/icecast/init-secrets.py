#!/usr/bin/env python3
"""Initialisation locale idempotente. Ne montre jamais les mots de passe."""
from pathlib import Path
import os
import secrets

root = Path(__file__).resolve().parents[2] / ".local/icecast/secrets"
root.mkdir(parents=True, exist_ok=True, mode=0o700)
root.chmod(0o700)
for name in ("admin-password", "admin-api-token", "source-a-password", "source-b-password"):
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
print("Secrets locaux prêts (répertoire privé, valeurs non affichées).")

