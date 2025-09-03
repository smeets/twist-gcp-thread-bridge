FROM rust:latest AS builder

# Install musl stuff
# RUN apt update && apt install -y musl-tools libssl-dev openssl && rm -rf /var/lib/apt/lists/*
# RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /app
COPY ./src ./src
COPY ./Cargo.toml ./
COPY ./Cargo.lock ./
RUN cargo build --release 
RUN ls -lah ./target/release
RUN strip ./target/release/twist-gcp-notify-channel

FROM gcr.io/distroless/cc-debian12 as release
WORKDIR /app
COPY --from=builder /app/target/release/twist-gcp-notify-channel ./twist-gcp-thread-bridge

