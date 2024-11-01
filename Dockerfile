# syntax=docker/dockerfile:1.4

ARG RUST_VERSION=1.72.0
ARG CARGO_BUILD_FEATURES=default

# AMD64 builder base
FROM --platform=${BUILDPLATFORM} blackdex/rust-musl:x86_64-musl-stable-${RUST_VERSION}-openssl3 AS base-amd64

ENV DEBIAN_FRONTEND=noninteractive
ENV CARGO_HOME=/root/.cargo
ENV PQ_LIB_DIR=/usr/local/musl/pq15/lib

RUN apt update && apt install -y \
    --no-install-recommends \
    git

RUN mkdir -pv "${CARGO_HOME}" && \
    rustup set profile minimal && \
    rustup target add x86_64-unknown-linux-musl

# ARM64 builder base
FROM --platform=${BUILDPLATFORM} blackdex/rust-musl:aarch64-musl-stable-${RUST_VERSION}-openssl3 AS base-arm64

ENV DEBIAN_FRONTEND=noninteractive
ENV CARGO_HOME=/root/.cargo
ENV PQ_LIB_DIR=/usr/local/musl/pq15/lib

RUN apt update && apt install -y \
    --no-install-recommends \
    git

RUN mkdir -pv "${CARGO_HOME}" && \
    rustup set profile minimal && \
    rustup target add aarch64-unknown-linux-musl

# AMD64 builder
FROM base-amd64 AS build-amd64

ARG CARGO_BUILD_FEATURES

WORKDIR /app

COPY Cargo.* .
COPY src ./src

RUN --mount=type=cache,target=${CARGO_HOME}/registry,id=${TARGETPLATFORM} \
    --mount=type=cache,target=/app/target,id=${TARGETPLATFORM} \
    cargo build --target=x86_64-unknown-linux-musl --features "${CARGO_BUILD_FEATURES}" --release; \
    mv target/x86_64-unknown-linux-musl/release/svt-agent .

# ARM64 builder
FROM base-arm64 AS build-arm64

ARG CARGO_BUILD_FEATURES

WORKDIR /app

COPY Cargo.* .
COPY src ./src

RUN --mount=type=cache,target=${CARGO_HOME}/registry,id=${TARGETPLATFORM} \
    --mount=type=cache,target=/app/target,id=${TARGETPLATFORM} \
    cargo build --target=aarch64-unknown-linux-musl --features "${CARGO_BUILD_FEATURES}" --release; \
    mv target/aarch64-unknown-linux-musl/release/svt-agent .

# Get target binary
FROM build-${TARGETARCH} AS build

# Final image
FROM cgr.dev/chainguard/static

WORKDIR /app

COPY ./ansible/ ./ansible
COPY --from=build /app/svt-agent /app
# The "nonroot" user does not have the necessary permissions to connect to the Docker socket.
#COPY --from=build --chown=nonroot:nonroot /app/svt-agent /app
USER root

ENTRYPOINT ["/app/svt-agent"]
