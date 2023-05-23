# syntax=docker/dockerfile:1.4
# https://github.com/LukeMathWalker/cargo-chef

ARG APP_NAME=svt-agent
ARG RUST_LOG=info
ARG RUST_BACKTRACE=0

FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY Cargo.* .
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS cacher
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --recipe-path recipe.json

FROM chef AS builder
COPY Cargo.* .
COPY src ./src
COPY --from=cacher /app/target target
COPY --from=cacher $CARGO_HOME $CARGO_HOME
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release

# FROM scratch
# FROM gcr.io/distroless/cc
# FROM gcr.io/distroless/cc-debian1
FROM debian:buster-slim AS final

WORKDIR /app

ARG APP_NAME
ARG RUST_LOG
ENV RUST_LOG=${RUST_LOG}
ARG RUST_BACKTRACE
ENV RUST_BACKTRACE=${RUST_BACKTRACE}

COPY --from=builder /app/target/release/${APP_NAME} /
COPY ./ansible/ ./ansible

ENTRYPOINT [ "/svt-agent" ]
