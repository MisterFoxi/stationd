# syntax=docker/dockerfile:1
FROM rust:1.99-slim-bookworm AS toolchain
ENV CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/build
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/* \
 && rustup component add rustfmt clippy \
 && rustup target add wasm32-unknown-unknown
# The CLI must match the exact wasm-bindgen crate version in Cargo.lock.
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-bindgen-target,target=/build,sharing=locked \
    cargo install --locked wasm-bindgen-cli --version 0.2.128 --root /usr/local
WORKDIR /src

FROM toolchain AS source
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ src/
COPY crates/ crates/
# Bundled manifests are embedded with include_str! by modules and registry tests.
COPY Doc/stationdeck/ Doc/stationdeck/
COPY plugins/station-player/ plugins/station-player/
COPY plugins/icecastd-deck/ plugins/icecastd-deck/
COPY proto/ proto/
COPY migrations/ migrations/
COPY config/stationdeck/ config/stationdeck/

FROM source AS build
ARG STATIONDECK_CARGO_FEATURES=""
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-web-target,target=/build,sharing=locked \
    cargo build --locked --release --target wasm32-unknown-unknown -p stationdeck --features "$STATIONDECK_CARGO_FEATURES" \
 && mkdir -p /out/pkg \
 && wasm-bindgen --target web --out-dir /out/pkg --out-name stationdeck /build/wasm32-unknown-unknown/release/stationdeck.wasm \
 && cp crates/stationdeck/web/index.html crates/stationdeck/web/boot.js crates/stationdeck/web/style.css /out/

FROM source AS checks
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-web-target,target=/build,sharing=locked \
    cargo fmt -p stationdeck -p stationdeck-ui -p stationdeck-services -p stationdeck-discovery -p stationdeck-demo -p stationdeck-grpc -p stationdeck-gateway -p stationdeck-config-ui -p stationdeck-icecast -p icecastd-deck -p stationdeck-control --check \
 && cargo test --locked -p stationdeck -p stationdeck-ui -p stationdeck-services -p stationdeck-discovery -p stationdeck-demo -p stationdeck-grpc -p stationdeck-config-ui -p stationdeck-icecast -p icecastd-deck \
 && cargo clippy --locked -p stationdeck -p stationdeck-ui -p stationdeck-services -p stationdeck-discovery -p stationdeck-demo -p stationdeck-grpc -p stationdeck-config-ui -p stationdeck-icecast -p icecastd-deck --all-targets -- -D warnings \
 && cargo clippy --locked -p stationdeck --target wasm32-unknown-unknown -- -D warnings \
 && cargo tree --locked -p stationdeck --target wasm32-unknown-unknown --prefix none > /tmp/stationdeck-dependencies \
 && ! grep -E '^(stationd |tonic |prost |prost-types )' /tmp/stationdeck-dependencies \
 && cargo tree --locked -p icecastd-deck --target wasm32-unknown-unknown --prefix none > /tmp/icecast-plugin-dependencies \
 && ! grep -E '^(stationdeck-icecast |gloo-net |tonic |prost |stationd )' /tmp/icecast-plugin-dependencies

FROM source AS gateway-build
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev && rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-web-target,target=/build,sharing=locked \
    cargo build --locked --release -p stationdeck-gateway --bin stationdeck-gateway \
 && mkdir -p /gateway-out && cp /build/release/stationdeck-gateway /gateway-out/

FROM gateway-build AS gateway-checks
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-web-target,target=/build,sharing=locked \
    cargo test --locked -p stationdeck-gateway \
 && cargo clippy --locked -p stationdeck-gateway --all-targets --no-deps -- -D warnings

FROM gateway-build AS fixture-build
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-web-target,target=/build,sharing=locked \
    cargo build --locked --release -p stationdeck-gateway --features test-fixture --bin stationdeck-grpc-fixture \
 && cp /build/release/stationdeck-grpc-fixture /gateway-out/

FROM debian:bookworm-slim AS gateway
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd -g 1000 deck && useradd -m -u 1000 -g deck deck \
 && mkdir /secrets /configuration-credentials && chown deck:deck /secrets /configuration-credentials
COPY --from=gateway-build /gateway-out/stationdeck-gateway /usr/local/bin/stationdeck-gateway
USER 1000:1000
HEALTHCHECK --interval=5s --timeout=2s CMD curl -fsS -o /dev/null http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["stationdeck-gateway"]

FROM gateway AS fixture
COPY --from=fixture-build /gateway-out/stationdeck-grpc-fixture /usr/local/bin/stationdeck-grpc-fixture
HEALTHCHECK NONE
ENTRYPOINT ["stationdeck-grpc-fixture"]

FROM nginxinc/nginx-unprivileged:1.28-alpine AS runtime
COPY docker/stationdeck/nginx.conf /etc/nginx/nginx.conf
COPY --from=build /out/ /usr/share/nginx/html/
USER 101
EXPOSE 8080
HEALTHCHECK --interval=5s --timeout=2s --start-period=5s CMD wget -q -O /dev/null http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["nginx", "-g", "daemon off;"]

FROM mcr.microsoft.com/playwright:v1.56.1-noble AS qa
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
WORKDIR /test
RUN npm install --prefix /test --no-audit --no-fund playwright@1.56.1 pngjs@7.0.0
COPY tests/stationdeck-web-smoke.mjs /test/stationdeck-web-smoke.mjs
USER 1000:1000
ENTRYPOINT ["node", "/test/stationdeck-web-smoke.mjs"]

FROM source AS control-build
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev && rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-control-target,target=/build,sharing=locked \
    cargo build --locked --release -p stationdeck-control \
 && mkdir -p /control-out && cp /build/release/stationdeck-control /control-out/

FROM control-build AS control-checks
RUN --mount=type=cache,id=stationdeck-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=stationdeck-control-target,target=/build,sharing=locked \
    cargo test --locked -p stationdeck-control \
 && cargo clippy --locked -p stationdeck-control --all-targets --no-deps -- -D warnings

FROM gateway AS control
USER root
RUN mkdir -p /data /author-secrets /agent-secrets /service-credentials && chown 1000:1000 /data /author-secrets /agent-secrets /service-credentials
COPY --from=control-build /control-out/stationdeck-control /usr/local/bin/stationdeck-control
USER 1000:1000
ENTRYPOINT ["stationdeck-control"]
COPY --chmod=0755 docker/stationdeck/apply-sd /usr/local/bin/stationdeck-apply-sd
