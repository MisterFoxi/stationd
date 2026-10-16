#!/usr/bin/env python3
"""Continuous, quiet test tones. No StationD, media files or NFS."""
import os
from pathlib import Path
import urllib.parse

label = os.environ.get("SOURCE_LABEL", "")
if label not in ("a", "b"):
    raise SystemExit("SOURCE_LABEL must be a or b")
frequency = 440 if label == "a" else 880
secret = (Path("/run/secrets") / f"source-{label}-password").read_text().strip()
if not secret:
    raise SystemExit("Missing source secret")
target = f"icecast://source:{urllib.parse.quote(secret, safe='')}@icecast:8000/test-{label}.mp3"
args = [
    "ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "quiet", "-re",
    "-f", "lavfi", "-i", f"sine=frequency={frequency}:sample_rate=44100",
    "-af", "volume=0.15", "-ac", "1", "-c:a", "libmp3lame", "-b:a", "64k",
    "-ice_name", f"StationD test {label.upper()} - {frequency} Hz",
    "-content_type", "audio/mpeg", "-f", "mp3", target,
]
# FFmpeg becomes PID 1 and receives Docker stop signals directly.
os.execvp(args[0], args)

