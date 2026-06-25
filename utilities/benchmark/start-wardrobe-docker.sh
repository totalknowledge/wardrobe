#!/usr/bin/env bash
set -euo pipefail

CONTAINER_NAME="${WARDROBE_BENCH_WARDROBE_CONTAINER:-wardrobe-benchmark-wardrobe}"
IMAGE="${WARDROBE_BENCH_WARDROBE_IMAGE:-rust:1}"
HOST_PORT="${WARDROBE_BENCH_WARDROBE_PORT:-24842}"
DATA_VOLUME="${WARDROBE_BENCH_WARDROBE_VOLUME:-wardrobe-benchmark-wardrobe-data}"
TARGET_VOLUME="${WARDROBE_BENCH_WARDROBE_TARGET_VOLUME:-wardrobe-benchmark-wardrobe-target}"
CARGO_REGISTRY_VOLUME="${WARDROBE_BENCH_CARGO_REGISTRY_VOLUME:-wardrobe-benchmark-cargo-registry}"
CARGO_GIT_VOLUME="${WARDROBE_BENCH_CARGO_GIT_VOLUME:-wardrobe-benchmark-cargo-git}"
CONNECTION_POOL_LIMIT="${WARDROBE_BENCH_WARDROBE_CONNECTION_POOL_LIMIT:-16}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

log_step() {
    printf '\n==> %s\n' "$1"
}

require_docker() {
    log_step "Checking for Docker"
    if ! command -v docker >/dev/null 2>&1; then
        echo "docker is required to start the Wardrobe benchmark server" >&2
        exit 1
    fi
    docker --version
}

container_running() {
    docker ps --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"
}

require_docker

log_step "Preparing Wardrobe benchmark container"
echo "Container name      : $CONTAINER_NAME"
echo "Image               : $IMAGE"
echo "Repo mount          : $REPO_ROOT -> /workspace"
echo "Host port           : 127.0.0.1:$HOST_PORT"
echo "Data volume         : $DATA_VOLUME"
echo "Target volume       : $TARGET_VOLUME"
echo "Cargo registry cache: $CARGO_REGISTRY_VOLUME"
echo "Cargo git cache     : $CARGO_GIT_VOLUME"
echo "Connection pool     : $CONNECTION_POOL_LIMIT active workers"

if container_running; then
    echo "Wardrobe benchmark container is already running: $CONTAINER_NAME"
else
    log_step "Resetting Wardrobe container"
    echo "Clearing stale container to apply fresh mount specifications..."
    docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true

    log_step "Creating Wardrobe container"
    echo "Docker may pull $IMAGE if it is not available locally."
    echo "Server command: cargo run --release -p wardrobe-server -- --data-dir /data/wardrobe --tcp-bind 0.0.0.0:24842 --connection-pool-limit $CONNECTION_POOL_LIMIT"
    MSYS_NO_PATHCONV=1 docker run -d \
        --name "$CONTAINER_NAME" \
        -w /workspace \
        -p "127.0.0.1:$HOST_PORT:24842" \
        -v "$REPO_ROOT:/workspace" \
        -v "$DATA_VOLUME:/data/wardrobe" \
        -v "$TARGET_VOLUME:/workspace/target" \
        -v "$CARGO_REGISTRY_VOLUME:/usr/local/cargo/registry" \
        -v "$CARGO_GIT_VOLUME:/usr/local/cargo/git" \
        "$IMAGE" \
        cargo run --release -p wardrobe-server -- --data-dir /data/wardrobe --tcp-bind 0.0.0.0:24842 --connection-pool-limit "$CONNECTION_POOL_LIMIT"
fi

log_step "Waiting for Wardrobe TCP server to start"
echo "Container: $CONTAINER_NAME"
echo "Readiness log marker: Wardrobe daemon listening on TCP"
echo "The first run may spend several minutes pulling the Rust image and compiling the workspace."
echo "Live logs: docker logs -f $CONTAINER_NAME"
WARDROBE_READY=0
for attempt in $(seq 1 120); do
    if docker logs "$CONTAINER_NAME" 2>&1 | grep -q "Wardrobe daemon listening on TCP"; then
        echo "Wardrobe TCP server is listening."
        WARDROBE_READY=1
        break
    fi
    if [ $((attempt % 5)) -eq 0 ]; then
        echo "Still waiting for Wardrobe after $((attempt * 2)) seconds. Recent logs:"
        docker logs --tail 12 "$CONTAINER_NAME" 2>&1 || true
    else
        echo "Attempt $attempt/120: Wardrobe is not listening yet."
    fi
    sleep 2
done

if [ "$WARDROBE_READY" != "1" ]; then
    echo "Wardrobe did not become ready in time. Recent logs:" >&2
    docker logs --tail 40 "$CONTAINER_NAME" 2>&1 || true
    exit 1
fi

if ! docker ps --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"; then
    echo "Wardrobe container died unexpectedly. Container log output below:" >&2
    docker logs "$CONTAINER_NAME"
    exit 1
fi

log_step "Ready"
cat <<EOF
Wardrobe benchmark server is ready.

Benchmark flags:
  --targets wardrobe-remote --wardrobe-remote-uri wardrobe://127.0.0.1:$HOST_PORT
EOF
