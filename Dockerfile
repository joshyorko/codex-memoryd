# syntax=docker/dockerfile:1

# ---- Builder ----------------------------------------------------------------
# Build a static-ish release binary. SQLite is bundled (compiled from source via
# the `bundled` feature), so the runtime image needs no libsqlite3.
FROM rust:1-bookworm AS builder

WORKDIR /build

# Cache dependencies: copy manifests first, build a stub, then the real source.
COPY Cargo.toml ./
COPY migrations ./migrations
# Create a stub lib/bin so `cargo build` can resolve+compile dependencies and
# cache them in a separate layer before the real sources are copied.
RUN mkdir -p src \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && cargo build --release --quiet || true

# Now copy the real source and build for real.
COPY src ./src
# Touch sources so cargo rebuilds them (stub timestamps are older).
RUN touch src/main.rs src/lib.rs \
    && cargo build --release \
    && strip target/release/codex-memoryd

# ---- Runtime ----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# curl is used by the container HEALTHCHECK; ca-certificates for safety.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --system --create-home --uid 10001 memoryd

COPY --from=builder /build/target/release/codex-memoryd /usr/local/bin/codex-memoryd

# Persistent storage lives under /data (mount a volume here).
ENV CODEX_MEMORYD_DB=/data/memory.db \
    CODEX_MEMORYD_BIND=0.0.0.0:8787 \
    CODEX_MEMORYD_LOG=info
RUN mkdir -p /data && chown memoryd:memoryd /data
VOLUME ["/data"]

USER memoryd
EXPOSE 8787

# Healthcheck hits the lightweight /healthz endpoint.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8787/healthz || exit 1

ENTRYPOINT ["codex-memoryd"]
CMD ["serve"]
