# syntax=docker/dockerfile:1.2

## global args
ARG APP_NAME=svt-agent

# use it only if you really need to squeeze the bytes
# note: alpine base already comes with ~5.61 MB (alpine 3.11)
ARG STRIP=0

ARG RUST_LOG=info

# if you want more information on panics
ARG RUST_BACKTRACE=0

# ARG BASE_IMAGE=ghcr.io/asaaki/rust-musl-cross:x86_64-musl
# ARG BASE_IMAGE=ekidd/rust-musl-builder:latest
ARG BASE_IMAGE=nwtgck/rust-musl-builder:1.67.0

#########################
# Builder layer
#########################

FROM ${BASE_IMAGE} AS builder
# base image might have changed it, so let's enforce root to be able to do system changes
USER root

ENV BUILD_CACHE_BUSTER="2022-05-30T00:00:00"
ENV DEB_PACKAGES="ca-certificates curl file git make patch wget xz-utils"

# @see https://github.com/moby/buildkit/blob/master/frontend/dockerfile/docs/experimental.md#example-cache-apt-packages
RUN rm -f /etc/apt/apt.conf.d/docker-clean \
 && echo 'Binary::apt::APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/keep-cache

RUN \
  --mount=type=cache,target=/var/cache/apt \
  --mount=type=cache,target=/var/lib/apt \
    echo "===== Build environment =====" \
 && uname -a \
 && echo "===== Dependencies =====" \
 && apt-get update \
 && apt-get install -y --no-install-recommends $DEB_PACKAGES \
 && echo "===== Toolchain =====" \
 && rustup --version \
 && cargo --version \
 && rustc --version \
 && echo "Rust builder image done."

#######################
# Build layer
#######################

FROM builder as build

# @see https://docs.docker.com/engine/reference/builder/#understand-how-arg-and-from-interact
ARG APP_NAME
ARG BUILD_MODE
ARG STRIP

# ENV RUSTFLAGS="-C target-feature=-crt-static"

WORKDIR /app

# COPY Cargo.* build.rs /app/
COPY Cargo.* /app/
COPY src /app/src

RUN \
  --mount=type=cache,target=/usr/local/cargo/registry \
  --mount=type=cache,target=/app/target \
  cargo fetch && \
  cargo install --root /app --target=x86_64-unknown-linux-musl --path .

RUN echo "SIZE ORIGIN:" && du -h bin/${APP_NAME}
# remove debug symbols
RUN [ "${STRIP}" = "1" ] && (echo "Stripping debug symbols ..."; strip bin/${APP_NAME}) || echo "No stripping enabled"
RUN echo "SIZE FINAL:" && du -h bin/${APP_NAME}

######################
# Base layer
######################

FROM alpine:3.17.2 as base
RUN apk --no-cache update && \
    apk --no-cache upgrade && \
    apk --no-cache add ca-certificates
WORKDIR /app

####################
# Run layer
####################

# This is why we do not want to use 'FROM scratch',
# otherwise the user within the container would be still root

# FROM base as run
# RUN addgroup -g 1001 appuser \
#  && adduser  -u 1001 -G appuser -H -D appuser
# USER 1001

#######################
# Assets
#######################

FROM base AS assets
ARG SV_MANAGER_TAG=agent-v0.3.3
RUN wget -O archive.tar.gz https://github.com/mfactory-lab/sv-manager/archive/refs/tags/${SV_MANAGER_TAG}.tar.gz \
  && tar -xvf archive.tar.gz --strip-components=1 \
  && mv inventory_example inventory \
  && rm archive.tar.gz

#######################
# Final image
#######################

# # not really recommended
# FROM scratch as production_binary
#
# ARG APP_NAME
# ARG RUST_LOG
# ENV RUST_LOG=${RUST_LOG}
# ARG RUST_BACKTRACE
# ENV RUST_BACKTRACE=${RUST_BACKTRACE}
#
# COPY --from=build /app/bin/${APP_NAME} /
# COPY --from=assets /app ./ansible
# COPY ./ansible/ ./ansible
#
# ENTRYPOINT [ "/svt-agent" ]

# recommended
FROM base as production

ARG APP_NAME
ARG RUST_LOG
ENV RUST_LOG=${RUST_LOG}
ARG RUST_BACKTRACE
ENV RUST_BACKTRACE=${RUST_BACKTRACE}

COPY --from=build /app/bin/${APP_NAME} /
# COPY --from=build --chown=appuser:appuser /app/bin/${APP_NAME} /
COPY --from=assets /app ./ansible
COPY ./ansible/ ./ansible

ENTRYPOINT [ "/svt-agent" ]
