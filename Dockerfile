FROM rust:1-bookworm AS build

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --package grok-proxy --bin proxy

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --user-group --create-home --system --skel /dev/null --home-dir /app --shell /usr/sbin/nologin grok

WORKDIR /app

COPY --from=build --chown=grok:grok /build/target/release/proxy /app/proxy

USER grok:grok

EXPOSE 8080

ENTRYPOINT ["./proxy"]
CMD ["serve", "--env", "production", "--hostname", "0.0.0.0", "--port", "8080"]
