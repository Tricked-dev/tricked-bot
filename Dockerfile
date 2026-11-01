FROM lukemathwalker/cargo-chef:latest-rust-1.99.1-slim-trixie AS chef
WORKDIR /tricked-bot

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /tricked-bot/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/tricked-bot/target \
    cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/tricked-bot/target \
    cargo build --release --bin tricked-bot && \
    cp target/release/tricked-bot /usr/local/bin/tricked-bot

# Templates and migrations are compiled in; only the tools the bot shells out to are needed at runtime.
FROM debian:trixie-slim AS runtime
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates ffmpeg qalc
COPY --from=builder /usr/local/bin/tricked-bot /usr/local/bin/tricked-bot
ENTRYPOINT ["/usr/local/bin/tricked-bot"]
