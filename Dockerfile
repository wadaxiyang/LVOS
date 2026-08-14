ARG LVOS_DOCKER_RUST_IMAGE=docker.m.daocloud.io/library/rust:1.94.1-bookworm
ARG LVOS_DOCKER_RUNTIME_IMAGE=docker.m.daocloud.io/library/debian:bookworm-slim

FROM ${LVOS_DOCKER_RUST_IMAGE} AS builder
ARG LVOS_CARGO_REGISTRY_INDEX=sparse+https://rsproxy.cn/index/
ENV CARGO_HTTP_MULTIPLEXING=false \
    CARGO_NET_RETRY=5
WORKDIR /src
COPY Cargo.server.toml Cargo.toml
COPY Cargo.server.lock Cargo.lock
COPY LICENSE ./
COPY apps/server apps/server
COPY crates/auth crates/auth
COPY crates/core crates/core
COPY crates/storage crates/storage
COPY crates/sync crates/sync
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    mkdir --parents /usr/local/cargo \
    && if [ "${LVOS_CARGO_REGISTRY_INDEX}" != "sparse+https://index.crates.io/" ]; then \
        printf '[source.crates-io]\nreplace-with = "lvos-build-source"\n[source.lvos-build-source]\nregistry = "%s"\n' "${LVOS_CARGO_REGISTRY_INDEX}" > /usr/local/cargo/config.toml; \
    fi \
    && cargo build --locked --release -p lvos-server

FROM ${LVOS_DOCKER_RUNTIME_IMAGE} AS runtime
RUN install --directory --owner=10001 --group=10001 /var/lib/lvos /var/lib/lvos/backups
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /src/target/release/lvos-server /usr/local/bin/lvos-server
USER 10001:10001
WORKDIR /var/lib/lvos
EXPOSE 7770
ENTRYPOINT ["/usr/local/bin/lvos-server"]
