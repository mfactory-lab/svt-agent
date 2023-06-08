# syntax=docker/dockerfile:1.4

ARG APP_NAME=svt-agent
ARG RUST_LOG=info
ARG RUST_BACKTRACE=0

##########################
## Build layer
##########################
# rust:1.69.0-slim-bullseye as of 2023/05/18
FROM rust:1.69.0-slim-bullseye as builder

ARG APP_NAME

# Optional and will be used just if you have matrix build with different platforms.
ARG TARGETPLATFORM

ENV CARGO_HOME=/usr/local/cargo
ENV SCCACHE_DIR=/usr/local/sccache
ENV SCCACHE_CACHE_SIZE="3G"

#ENV CARGO_INCREMENTAL=0

WORKDIR /app

RUN apt update && apt install -y libssl-dev ca-certificates pkg-config

RUN --mount=type=cache,target=${CARGO_HOME}/registry,id=${TARGETPLATFORM} \
    cargo install cargo-strip sccache

ENV CARGO_BUILD_RUSTC_WRAPPER=/usr/local/cargo/bin/sccache

# RUN apt update && apt install -y --no-install-recommends libopus-dev libssl-dev pkg-config

COPY Cargo.* .
COPY src ./src

RUN --mount=type=cache,target=${CARGO_HOME}/registry,id=${TARGETPLATFORM} \
    --mount=type=cache,target=${SCCACHE_DIR},id=${TARGETPLATFORM} \
    --mount=type=cache,target=/app/target,id=${TARGETPLATFORM} \
    cargo --locked build --release

RUN cargo strip && sccache --show-stats

#RUN \
#  --mount=type=cache,target=/usr/local/cargo/registry \
#  --mount=type=cache,target=/app/target \
#  cargo install --root /app --path .

# do a release build
#RUN cargo build --release
#RUN strip target/release/${APP_NAME}

# RUN cargo install --root /app --target=x86_64-unknown-linux-musl --path .
# RUN cargo install --root /app --path .

#######################
# Final image
#######################

FROM debian:buster-slim AS final
#FROM debian:bullseye-slim as final
#FROM alpine:3.18 as final

WORKDIR /app

ARG APP_NAME
ARG RUST_LOG
ENV RUST_LOG=${RUST_LOG}
ARG RUST_BACKTRACE
ENV RUST_BACKTRACE=${RUST_BACKTRACE}

COPY --from=builder /app/target/release/${APP_NAME} /
COPY ./ansible/ ./ansible

#ENTRYPOINT ["/tini", "--"]
ENTRYPOINT [ "/svt-agent" ]
