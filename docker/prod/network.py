#!/usr/bin/env python3
"""Refresh only server.grpc_bind from the node's detected IPv4 address."""
import ipaddress
import re
import sys
import tomllib


def configure(text, host):
    ip = ipaddress.IPv4Address(host)
    if ip.is_loopback or ip.is_unspecified or ip.is_multicast:
        raise ValueError("the detected address must belong to the node's network interface")
    config = tomllib.loads(text)
    address = config["server"]["grpc_bind"]
    old_host, separator, port = address.rpartition(":")
    if not separator or not port.isascii() or not port.isdigit() or not 1 <= int(port) <= 65535:
        raise ValueError("[server] grpc_bind must contain a valid port")
    ipaddress.ip_address(old_host.strip("[]"))
    # Keep the port and every other setting/comment; TOML is parsed before and after.
    section = re.search(r"(?m)^\[server\][ \t]*(?:#[^\n]*)?\n(?:(?!^\[).*(?:\n|$))*", text)
    if not section:
        raise ValueError("a plain [server] section is required")
    updated, count = re.subn(r"""(?m)^(grpc_bind\s*=\s*)(?:"[^"\n]*"|'[^'\n]*')""",
                            lambda m: m[1] + '"' + str(ip) + ":" + port + '"',
                            section[0])
    if count != 1:
        raise ValueError("expected exactly one server.grpc_bind assignment")
    result = text[:section.start()] + updated + text[section.end():]
    tomllib.loads(result)
    return result


if __name__ == "__main__":
    try:
        with open("/stationd-config.toml", encoding="utf-8") as stream:
            sys.stdout.write(configure(stream.read(), sys.argv[1]))
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        sys.exit(f"stationd network: {error}")