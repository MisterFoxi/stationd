#!/usr/bin/env python3
"""Generate the host unit with mount dependencies from Compose's validated plan."""
import sys


def quote(value):
    if any(c in value for c in "\r\n\0"):
        raise ValueError("invalid systemd path")
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%") + '"'


def unit(directory, sources):
    paths = " ".join(quote(p) for p in dict.fromkeys([directory, *sources]))
    script = quote(directory + "/scripts/service.sh").replace("$", "$$")
    return f"""[Unit]
Description=stationd station (Docker Compose)
Requires=docker.service
PartOf=docker.service
After=docker.service network-online.target remote-fs.target
Wants=network-online.target
StartLimitIntervalSec=0
RequiresMountsFor={paths}

[Service]
Type=oneshot
RemainAfterExit=yes
Restart=on-failure
RestartSec=10
ExecStart=/bin/bash {script} start
ExecStop=/bin/bash {script} stop
TimeoutStartSec=180
TimeoutStopSec=60

[Install]
WantedBy=multi-user.target
"""


if __name__ == "__main__":
    with open("/stationd-plan", encoding="utf-8") as plan:
        sources = [line.rstrip("\n").split("\t")[1] for line in plan if line.strip()]
    print(unit(sys.argv[1], sources), end="")