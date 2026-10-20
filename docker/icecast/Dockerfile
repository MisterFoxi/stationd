# syntax=docker/dockerfile:1
FROM rust:1.90-slim-bookworm AS config-build
WORKDIR /workspace/tools/icecast-config
COPY tools/icecast-config/ ./
COPY src/geoip.rs /workspace/src/geoip.rs
RUN cargo build --release --locked

FROM rust:1.99-slim-bookworm AS admin-build
RUN apt-get update \
 && apt-get install -y --no-install-recommends protobuf-compiler \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace/crates/stationd-admin
COPY crates/stationd-admin/ ./
COPY crates/stationdeck-discovery/ /workspace/crates/stationdeck-discovery/
COPY tools/icecast-config/ /workspace/tools/icecast-config/
COPY src/geoip.rs src/listener_snapshot.rs src/listener_history.rs src/plugin_db.rs /workspace/src/
ENV CARGO_BUILD_JOBS=2
# Rebuild local crates on source changes; retain compiled third-party dependencies.
RUN --mount=type=cache,id=stationd-v1-admin-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationd-v1-admin-target,target=/workspace/crates/stationd-admin/target,sharing=locked \
    cargo clean --release -p stationd-admin -p station-icecast-config \
 && cargo build --release --locked \
 && mkdir -p /out \
 && cp target/release/station-admin target/release/stationctl target/release/station-router target/release/station-watch /out/

FROM rust:1.99-slim-bookworm AS plugin-build
RUN rustup target add wasm32-unknown-unknown
WORKDIR /workspace/plugins/listener-stats-wasm
COPY plugins/listener-stats-wasm/ ./
RUN cargo build --release --locked --target wasm32-unknown-unknown

FROM ubuntu:26.04 AS standalone
ARG ICECAST_VERSION=2.5.0-1
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends "icecast2=${ICECAST_VERSION}" curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=config-build /workspace/tools/icecast-config/target/release/station-icecast-config /usr/local/bin/
COPY --chmod=0755 docker/icecast/start.sh /usr/local/bin/start-icecast
USER icecast2
EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/start-icecast"]

FROM ubuntu:26.04 AS test-tools
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends python3 ffmpeg ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=admin-build /out/stationctl /usr/local/bin/stationctl
COPY tests/icecast-smoke.py /test/icecast-smoke.py
COPY docker/icecast/live-source.py /test/live-source.py
ENTRYPOINT ["python3", "/test/icecast-smoke.py"]

FROM ubuntu:26.04 AS administration
COPY --from=admin-build /out/station-admin /out/stationctl /out/station-router /usr/local/bin/
COPY --from=plugin-build /workspace/plugins/listener-stats-wasm/target/wasm32-unknown-unknown/release/listener_stats_wasm.wasm /opt/stationd/plugins/listener_stats_wasm.wasm
RUN mkdir -p /var/lib/station-router /var/lib/station-admin/plugins && chown -R nobody:nogroup /var/lib/station-router /var/lib/station-admin
ENV RUST_LOG=warn
USER nobody
ENTRYPOINT ["station-admin"]
CMD ["--bind", "0.0.0.0:50061"]

# Reusable recovery helper, independent of running station/source containers.
FROM ubuntu:26.04 AS recovery-tools
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends python3 ffmpeg ca-certificates \
 && rm -rf /var/lib/apt/lists/*


# Independent read-only supervision with its own authenticated gRPC API.
FROM ubuntu:26.04 AS monitoring
COPY --from=admin-build /out/station-watch /out/stationctl /usr/local/bin/
RUN mkdir -p /var/lib/station-monitor && chown nobody:nogroup /var/lib/station-monitor
USER nobody
ENTRYPOINT ["station-watch"]

# Agent compiled from this checkout: no prebuilt local image is required.
FROM rust:1.99-slim-bookworm AS control-build
ENV CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/build
RUN apt-get update \
 && apt-get install -y --no-install-recommends protobuf-compiler pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY crates/ crates/
COPY src/ src/
COPY plugins/station-player/ plugins/station-player/
COPY plugins/icecastd-deck/ plugins/icecastd-deck/
COPY proto/ proto/
COPY migrations/ migrations/
COPY config/stationdeck/ config/stationdeck/
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-control-target,target=/build,sharing=locked \
    cargo build --locked --release -p stationdeck-control -p stationdeck-discovery \
 && mkdir -p /control-out && cp /build/release/stationdeck-control /build/release/stationdeck-discovery /control-out/

# Normal V1 server always contains and starts its configuration agent.
FROM standalone AS server
USER root
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /data /managed /routing /usr/local/lib/stationdeck \
 && chown 1000:1000 /data /managed /routing
COPY --from=control-build /control-out/stationdeck-control /control-out/stationdeck-discovery /usr/local/bin/
COPY docker/stationdeck/icecast-managed.py docker/stationdeck/icecast_routing_proxy.py /usr/local/lib/stationdeck/
COPY --chmod=0755 docker/stationdeck/apply-icecast /usr/local/bin/stationdeck-apply-icecast
USER 1000:1000
ENTRYPOINT ["python3", "/usr/local/lib/stationdeck/icecast-managed.py"]
