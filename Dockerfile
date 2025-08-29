FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /tricked-bot

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder 
COPY --from=planner /tricked-bot/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/tricked-bot/target \
    cargo chef cook --release --recipe-path recipe.json
# Build tricked-botlication
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/tricked-bot/target \
    cargo build --release --bin tricked-bot && \
    cp /tricked-bot/target/release/tricked-bot /usr/local/bin/tricked-bot

# We do not need the Rust toolchain to run the binary!
FROM debian:bookworm-slim AS runtime
WORKDIR /tricked-bot
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && \
    apt-get install -y --no-install-recommends libssl3 ca-certificates
COPY --from=chef /etc/ssl/certs /etc/ssl/certs
COPY --from=builder /usr/local/bin/tricked-bot /usr/local/bin
ENTRYPOINT ["/usr/local/bin/tricked-bot"]