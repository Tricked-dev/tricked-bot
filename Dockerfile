FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /tricked-bot

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder 
COPY --from=planner /tricked-bot/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build tricked-botlication
COPY . .
RUN cargo build --release --bin tricked-bot

# We do not need the Rust toolchain to run the binary!
FROM debian:bookworm-slim AS runtime
WORKDIR /tricked-bot
RUN apt-get update && \
    apt-get install -y --no-install-recommends libssl3 ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=chef /etc/ssl/certs /etc/ssl/certs
COPY --from=builder /tricked-bot/target/release/tricked-bot /usr/local/bin
ENTRYPOINT ["/usr/local/bin/tricked-bot"]