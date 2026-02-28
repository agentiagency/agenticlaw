FROM rust:1.85-slim AS build

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release && strip target/release/agenticlaw

FROM debian:bookworm-slim

RUN groupadd -g 1000 agenticlaw && useradd -u 1000 -g agenticlaw -s /bin/sh agenticlaw
COPY --from=build /build/target/release/agenticlaw /usr/local/bin/agenticlaw

USER 1000
ENTRYPOINT ["agenticlaw"]
