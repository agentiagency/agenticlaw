# Multi-stage Dockerfile for agenticlaw agent containers
# Build from repo root: docker build --build-arg ROLE=READ -t agenticlaw-read -f agenticlaw/operator/Dockerfile .
#
# Stage 1: Build agenticlaw from agenticlaw workspace
# Stage 2: Build REAL protectgateway from protectgateway_public
# Stage 3: Minimal runtime with baked-in policy

# ── Stage 1: Build agenticlaw ──
FROM rust:latest AS agenticlaw-builder

WORKDIR /src/agenticlaw
COPY agenticlaw/Cargo.toml agenticlaw/Cargo.lock ./
COPY agenticlaw/crates/ crates/
COPY agenticlaw/src/ src/

RUN cargo build --release -p agenticlaw-gateway \
    && cp target/release/agenticlaw /usr/local/bin/

# ── Stage 2: Build REAL protectgateway ──
FROM rust:latest AS pg-builder

WORKDIR /src/protectgateway
COPY protectgateway_public/Cargo.toml protectgateway_public/Cargo.lock ./
COPY protectgateway_public/src/ src/
COPY protectgateway_public/tools/ tools/

RUN cargo build --release --bin protectgateway \
    && cp target/release/protectgateway /usr/local/bin/

# ── Stage 3: Runtime ──
FROM debian:trixie-slim AS agent

ARG ROLE=READ

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget jq \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd -r agenticlaw && useradd -r -g agenticlaw -d /workspace -s /bin/false agenticlaw

# Rustclaw from stage 1
COPY --from=agenticlaw-builder /usr/local/bin/agenticlaw /usr/local/bin/agenticlaw

# REAL protectgateway from stage 2
COPY --from=pg-builder /usr/local/bin/protectgateway /usr/local/bin/protectgateway

# Bake in policy (immutable layer)
COPY agenticlaw/operator/policies/${ROLE}.json /etc/agenticlaw/policy.json

# Protectgateway config
COPY agenticlaw/operator/pg-config.yaml /etc/agenticlaw/pg-config.yaml

RUN mkdir -p /etc/agenticlaw/sub-policies /workspace /var/log/agenticlaw \
    && chown -R agenticlaw:agenticlaw /workspace

COPY agenticlaw/operator/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV ROLE=${ROLE} \
    RUSTCLAW_PORT=18790 \
    PROTECT_PORT=18789 \
    RUST_LOG=info

EXPOSE 18789

LABEL org.opencontainers.image.title="agenticlaw-${ROLE}" \
      org.opencontainers.image.description="Rustclaw agent with ${ROLE} policy" \
      com.agentiagency.role="${ROLE}"

WORKDIR /workspace
ENTRYPOINT ["/entrypoint.sh"]
