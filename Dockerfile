FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS cacher
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

FROM chef AS builder
COPY . .
COPY --from=cacher /app/target target
COPY --from=cacher /usr/local/cargo /usr/local/cargo
RUN cargo build --release

FROM chef AS sv_manager

ARG SV_MANAGER_VERSION=latest

RUN curl -sLO https://github.com/mfactory-lab/sv-manager/archive/refs/tags/${SV_MANAGER_VERSION}.tar.gz \
  && tar -xvf latest.tar.gz --strip-components=1 \
  && rm latest.tar.gz \
  && mv inventory_example inventory

#FROM gcr.io/distroless/cc
#FROM gcr.io/distroless/cc-debian1
#FROM debian:bullseye-slim
FROM scratch

WORKDIR app

COPY --from=builder /app/target/release/svt-agent /usr/local/bin/svt-agent
COPY --from=sv_manager /app ./ansible
COPY ./ansible/ ./ansible

RUN mkdir -p logs

ENV RUST_LOG=info
ENV RUST_BACKTRACE=1

ENTRYPOINT ["/usr/local/bin/svt-agent"]
