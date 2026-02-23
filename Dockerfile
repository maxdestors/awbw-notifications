FROM rust:1.92-bookworm AS builder
WORKDIR /app

# Build dependencies only so we can cache this layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
 && echo "fn main(){}" > src/main.rs \
 && cargo build --release --locked \
 && rm -rf src

# Build with source code
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim AS production
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/awbw_notifier /usr/local/bin/awbw_notifier
ENV PORT=8080

CMD ["/usr/local/bin/awbw_notifier"]
