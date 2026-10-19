ARG BASE_IMAGE=node:22-bookworm
FROM ${BASE_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends iproute2 iptables curl jq ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /lab
