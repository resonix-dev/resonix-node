# Build stage
FROM rust:1.89 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

# Runtime stage
FROM debian:bullseye-slim
WORKDIR /app
COPY --from=builder /app/target/release/resonix-node /app/resonix-node

COPY assets/ /app/assets/

ENV RUST_LOG=info
EXPOSE 0-65535
CMD ["/app/resonix-node"]
