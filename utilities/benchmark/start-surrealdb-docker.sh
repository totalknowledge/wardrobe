#!/usr/bin/env bash
set -euo pipefail

CONTAINER_NAME="${WARDROBE_BENCH_SURREAL_CONTAINER:-wardrobe-benchmark-surrealdb}"
IMAGE="${WARDROBE_BENCH_SURREAL_IMAGE:-surrealdb/surrealdb:v2.1}"
HOST_PORT="${WARDROBE_BENCH_SURREAL_PORT:-8000}"
DATABASE_EXPLICIT="${WARDROBE_BENCH_SURREAL_DATABASE+x}"
BENCHMARK_USER_EXPLICIT="${WARDROBE_BENCH_SURREAL_USER+x}"
BENCHMARK_PASSWORD_EXPLICIT="${WARDROBE_BENCH_SURREAL_PASSWORD+x}"
DATABASE="${WARDROBE_BENCH_SURREAL_DATABASE:-wardrobe_benchmark}"
NS="${WARDROBE_BENCH_SURREAL_NS:-wardrobe_benchmark}"
BENCHMARK_USER="${WARDROBE_BENCH_SURREAL_USER:-wardrobe_benchmark}"
BENCHMARK_PASSWORD="${WARDROBE_BENCH_SURREAL_PASSWORD:-wardrobe_benchmark}"
DATA_VOLUME="${WARDROBE_BENCH_SURREAL_VOLUME:-wardrobe-benchmark-surrealdb-data}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CREDENTIALS_FILE="${WARDROBE_BENCH_SURREAL_CREDENTIALS_FILE:-$WORKSPACE_ROOT/target/wardrobe-benchmark/surrealdb-credentials.env}"

log_step() {
    printf '\n==> %s\n' "$1"
}

require_docker() {
    log_step "Checking for Docker"
    if ! command -v docker >/dev/null 2>&1; then
        echo "docker is required to start the SurrealDB benchmark server" >&2
        exit 1
    fi
    docker --version
}

container_exists() {
    docker ps -a --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"
}

container_running() {
    docker ps --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"
}

wait_for_surrealdb() {
    log_step "Waiting for SurrealDB to accept connections"
    echo "Container: $CONTAINER_NAME"
    echo "Probe: curl -s -u $BENCHMARK_USER:*** http://127.0.0.1:$HOST_PORT/health"
    for attempt in $(seq 1 90); do
        if curl -s -u "$BENCHMARK_USER:$BENCHMARK_PASSWORD" "http://127.0.0.1:$HOST_PORT/health" >/dev/null 2>&1; then
            echo "SurrealDB is accepting connections."
            return 0
        fi
        if [ $((attempt % 10)) -eq 0 ]; then
            echo "Still waiting for SurrealDB after $((attempt * 2)) seconds. Recent logs:"
            docker logs --tail 8 "$CONTAINER_NAME" 2>&1 || true
        else
            echo "Attempt $attempt/90: SurrealDB is not ready yet."
        fi
        sleep 2
    done
    echo "SurrealDB did not become ready in time. Inspect logs with: docker logs $CONTAINER_NAME" >&2
    exit 1
}

write_credentials_file() {
    log_step "Writing benchmark credential fallback"
    mkdir -p "$(dirname "$CREDENTIALS_FILE")"
    (
        umask 077
        {
            printf 'WARDROBE_BENCH_SURREAL_USER=%s\n' "$BENCHMARK_USER"
            printf 'WARDROBE_BENCH_SURREAL_PASSWORD=%s\n' "$BENCHMARK_PASSWORD"
        } > "$CREDENTIALS_FILE"
    )
    echo "Wrote credentials to $CREDENTIALS_FILE"
}

require_docker

log_step "Preparing SurrealDB benchmark container"
echo "Container name : $CONTAINER_NAME"
echo "Image          : $IMAGE"
echo "Host port      : 127.0.0.1:$HOST_PORT"
echo "Namespace      : $NS"
echo "Database       : $DATABASE"
echo "Benchmark user : $BENCHMARK_USER"
echo "Data volume    : $DATA_VOLUME"

if container_running; then
    echo "SurrealDB benchmark container is already running: $CONTAINER_NAME"
elif container_exists; then
    log_step "Starting existing SurrealDB container"
    if ! docker start "$CONTAINER_NAME" >/dev/null 2>&1; then
        echo "Existing container failed to start. Recreating container with root permissions..."
        docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
        log_step "Creating SurrealDB container"
        docker run -d \
            --name "$CONTAINER_NAME" \
            -u 0:0 \
            -p "127.0.0.1:$HOST_PORT:8000" \
            -v "$DATA_VOLUME:/data" \
            "$IMAGE" start \
            --user "$BENCHMARK_USER" \
            --pass "$BENCHMARK_PASSWORD" \
            --bind "0.0.0.0:8000" \
            "rocksdb:/data/surreal.db"
    fi
else
    log_step "Creating SurrealDB container"
    echo "Docker may pull $IMAGE if it is not available locally."
    docker run -d \
        --name "$CONTAINER_NAME" \
        -u 0:0 \
        -p "127.0.0.1:$HOST_PORT:8000" \
        -v "$DATA_VOLUME:/data" \
        "$IMAGE" start \
        --user "$BENCHMARK_USER" \
        --pass "$BENCHMARK_PASSWORD" \
        --bind "0.0.0.0:8000" \
        "rocksdb:/data/surreal.db"
fi

wait_for_surrealdb
write_credentials_file

log_step "Ready"
cat <<EOF
SurrealDB benchmark server is ready.

Benchmark flags:
  --targets surrealdb --surreal-uri http://127.0.0.1:$HOST_PORT --surreal-ns $NS --surreal-db $DATABASE
EOF
