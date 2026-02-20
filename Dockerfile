# Multi-stage build for burst-daemon
FROM rust:latest AS builder

WORKDIR /usr/src/burst
COPY . .

RUN cargo build --release -p burst-daemon

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash burst

COPY --from=builder /usr/src/burst/target/release/burst-daemon /usr/local/bin/burst-daemon
COPY --from=builder /usr/src/burst/testnet.toml /home/burst/testnet.toml

RUN mkdir -p /home/burst/data && chown -R burst:burst /home/burst

USER burst
WORKDIR /home/burst

# P2P (test network default: 17076), RPC (7077), WebSocket (7078)
EXPOSE 17076 7077 7078

VOLUME ["/home/burst/data"]

ENTRYPOINT ["burst-daemon"]
CMD ["--data-dir", "/home/burst/data", "node", "run"]
