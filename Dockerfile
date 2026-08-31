# syntax=docker/dockerfile:1

# --- Build stage ---
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin rocket-mem

# --- Runtime stage ---
# Same Debian base family as the builder (bookworm), so the glibc the binary was linked
# against matches what's actually present here.
FROM debian:bookworm-slim
RUN useradd --system --create-home --shell /usr/sbin/nologin rocket-mem
COPY --from=builder /build/target/release/rocket-mem /usr/local/bin/rocket-mem
USER rocket-mem
WORKDIR /home/rocket-mem

# RESP, RMP, and Prometheus metrics -- matching Config's defaults
# (see docs/config-reference.md).
EXPOSE 6379 6380 9121

# Binds to 0.0.0.0 inside the container by default -- the image's whole point is to be reached
# from outside its own network namespace, unlike the loopback-only defaults a bare `cargo run`
# uses on a host. An operator overriding ROCKET_MEM_ADDR etc. still works normally.
ENV ROCKET_MEM_ADDR=0.0.0.0:6379
ENV ROCKET_MEM_RMP_ADDR=0.0.0.0:6380
ENV ROCKET_MEM_METRICS_ADDR=0.0.0.0:9121

ENTRYPOINT ["/usr/local/bin/rocket-mem"]
