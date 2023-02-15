# Using the `rust-musl-builder` as base image, instead of
# the official Rust toolchain
# FROM clux/muslrust:stable AS chef
# USER root
# RUN cargo install cargo-chef
# WORKDIR /app

FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
# RUN cargo chef cook --release --target x86_64-unknown-linux-musl --recipe-path recipe.json
COPY . .
# add logs folder here... (`RUN mkdir` doesnt work in the scratch image)
RUN mkdir -p logs
RUN cargo build --release
# RUN cargo build --release --target x86_64-unknown-linux-musl

FROM chef AS sv_manager
ARG SV_MANAGER_TAG=agent
RUN curl -sLO https://github.com/mfactory-lab/sv-manager/archive/refs/tags/${SV_MANAGER_TAG}.tar.gz \
  && tar -xvf latest.tar.gz --strip-components=1 \
  && rm latest.tar.gz \
  && mv inventory_example inventory

FROM gcr.io/distroless/cc
# FROM gcr.io/distroless/cc-debian1
# FROM debian:bullseye-slim
# FROM scratch

WORKDIR app

COPY --from=builder /app/target/release/svt-agent /usr/local/bin/svt-agent
# COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/svt-agent /usr/local/bin/
COPY --from=builder /app/logs ./
COPY --from=sv_manager /app ./ansible
COPY ./ansible/ ./ansible

ENV RUST_LOG=info
ENV RUST_BACKTRACE=1

ENTRYPOINT ["/usr/local/bin/svt-agent"]
