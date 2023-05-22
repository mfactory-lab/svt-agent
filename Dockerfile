# syntax=docker/dockerfile:1.4

ARG APP_NAME=svt-agent
ARG RUST_LOG=info
ARG RUST_BACKTRACE=0

##########################
## Build layer
##########################

FROM rust:slim as builder

## This is important, see https://github.com/rust-lang/docker-rust/issues/85
#ENV RUSTFLAGS="-C target-feature=-crt-static"
#
## if needed, add additional dependencies here
#RUN apk add --no-cache build-base openssl-dev

RUN apt update && apt install -y --no-install-recommends libopus-dev libssl-dev pkg-config

WORKDIR /app

COPY Cargo.* .
COPY src ./src

RUN \
  --mount=type=cache,target=/usr/local/cargo/registry \
  --mount=type=cache,target=/app/target \
  cargo install --root /app --path .

# do a release build
#RUN cargo build --release
#RUN strip target/release/${APP_NAME}

# RUN cargo install --root /app --target=x86_64-unknown-linux-musl --path .
# RUN cargo install --root /app --path .

#######################
# Final image
#######################

FROM debian:bullseye-slim as final
#FROM alpine:3.18 as final

WORKDIR /app

ARG APP_NAME
ARG RUST_LOG
ENV RUST_LOG=${RUST_LOG}
ARG RUST_BACKTRACE
ENV RUST_BACKTRACE=${RUST_BACKTRACE}

COPY --from=builder /app/bin/${APP_NAME} /
COPY ./ansible/ ./ansible

#ENTRYPOINT ["/tini", "--"]
ENTRYPOINT [ "/svt-agent" ]
