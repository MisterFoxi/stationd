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
COPY tools/icecast-config/ /workspace/tools/icecast-config/
COPY src/geoip.rs src/listener_snapshot.rs src/listener_history.rs src/plugin_db.rs /workspace/src/
ENV CARGO_BUILD_JOBS=2
RUN cargo build --release --locked

FROM rust:1.99-slim-bookworm AS plugin-build
RUN rustup target add wasm32-unknown-unknown
WORKDIR /workspace/plugins/listener-stats-wasm
COPY plugins/listener-stats-wasm/ ./
RUN cargo build --release --locked --target wasm32-unknown-unknown

FROM ubuntu:26.04 AS server
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
COPY --from=admin-build /workspace/crates/stationd-admin/target/release/stationctl /usr/local/bin/stationctl
COPY tests/icecast-smoke.py /test/icecast-smoke.py
COPY docker/icecast/live-source.py /test/live-source.py
ENTRYPOINT ["python3", "/test/icecast-smoke.py"]

FROM ubuntu:26.04 AS administration
COPY --from=admin-build /workspace/crates/stationd-admin/target/release/station-admin /workspace/crates/stationd-admin/target/release/stationctl /workspace/crates/stationd-admin/target/release/station-router /usr/local/bin/
COPY --from=plugin-build /workspace/plugins/listener-stats-wasm/target/wasm32-unknown-unknown/release/listener_stats_wasm.wasm /opt/stationd/plugins/listener_stats_wasm.wasm
RUN mkdir -p /var/lib/station-router /var/lib/station-admin/plugins && chown -R nobody:nogroup /var/lib/station-router /var/lib/station-admin
ENV RUST_LOG=warn
USER nobody
ENTRYPOINT ["station-admin"]
CMD ["--bind", "0.0.0.0:50061"]
