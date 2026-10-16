# syntax=docker/dockerfile:1
# Service commun V1, indépendant du conteneur historique StationD.
FROM rust:1.90-slim-bookworm AS config-build
WORKDIR /build
COPY tools/icecast-config/ ./
# include_str! du test n'est pas compilé en release.
RUN cargo build --release --locked

FROM rust:1.90-slim-bookworm AS admin-build
RUN apt-get update \
 && apt-get install -y --no-install-recommends protobuf-compiler \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /admin
COPY crates/stationd-admin/ ./
RUN cargo build --release --locked

FROM ubuntu:26.04 AS server
ARG ICECAST_VERSION=2.5.0-1
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends "icecast2=${ICECAST_VERSION}" curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=config-build /build/target/release/station-icecast-config /usr/local/bin/
COPY --chmod=0755 docker/icecast/start.sh /usr/local/bin/start-icecast
USER icecast2
EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/start-icecast"]

FROM ubuntu:26.04 AS test-tools
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends python3 ffmpeg ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=admin-build /admin/target/release/stationctl /usr/local/bin/stationctl
COPY tests/icecast-smoke.py /test/icecast-smoke.py
COPY docker/icecast/live-source.py /test/live-source.py
ENTRYPOINT ["python3", "/test/icecast-smoke.py"]


FROM ubuntu:26.04 AS administration
COPY --from=admin-build /admin/target/release/station-admin /admin/target/release/stationctl /usr/local/bin/
USER nobody
ENTRYPOINT ["station-admin"]
CMD ["--bind", "0.0.0.0:50061"]
