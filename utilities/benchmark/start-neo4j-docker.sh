#!/usr/bin/env bash
set -euo pipefail

CONTAINER_NAME="${WARDROBE_BENCH_NEO4J_CONTAINER:-wardrobe-benchmark-neo4j}"
IMAGE="${WARDROBE_BENCH_NEO4J_IMAGE:-neo4j:5.26-community}"
HOST_BOLT_PORT="${WARDROBE_BENCH_NEO4J_BOLT_PORT:-7687}"
HOST_HTTP_PORT="${WARDROBE_BENCH_NEO4J_HTTP_PORT:-7474}"
DATABASE="${WARDROBE_BENCH_NEO4J_DATABASE:-neo4j}"
NEO4J_USER="${WARDROBE_BENCH_NEO4J_USER:-neo4j}"
NEO4J_PASSWORD="${WARDROBE_BENCH_NEO4J_PASSWORD:-wardrobe_benchmark}"
DATA_VOLUME="${WARDROBE_BENCH_NEO4J_VOLUME:-wardrobe-benchmark-neo4j-data}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CREDENTIALS_FILE="${WARDROBE_BENCH_NEO4J_CREDENTIALS_FILE:-$WORKSPACE_ROOT/target/wardrobe-benchmark/neo4j-credentials.env}"

log_step() {
    printf '\n==> %s\n' "$1"
}

require_docker() {
    log_step "Checking for Docker"
    if ! command -v docker >/dev/null 2>&1; then
        echo "docker is required to start the Neo4j benchmark server" >&2
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

wait_for_neo4j() {
    log_step "Waiting for Neo4j to accept Bolt connections"
    echo "Container: $CONTAINER_NAME"
    echo "Probe: docker exec $CONTAINER_NAME cypher-shell -a bolt://127.0.0.1:7687 -u $NEO4J_USER -p *** RETURN 1"
    for attempt in $(seq 1 90); do
        if docker exec "$CONTAINER_NAME" cypher-shell -a bolt://127.0.0.1:7687 -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" "RETURN 1" >/dev/null 2>&1; then
            echo "Neo4j is accepting Bolt connections."
            return 0
        fi
        if [ $((attempt % 10)) -eq 0 ]; then
            echo "Still waiting for Neo4j after $((attempt * 2)) seconds. Recent logs:"
            docker logs --tail 8 "$CONTAINER_NAME" 2>&1 || true
        else
            echo "Attempt $attempt/90: Neo4j is not ready yet."
        fi
        sleep 2
    done
    echo "Neo4j did not become ready in time. Inspect logs with: docker logs $CONTAINER_NAME" >&2
    exit 1
}

write_credentials_file() {
    log_step "Writing benchmark credential fallback"
    mkdir -p "$(dirname "$CREDENTIALS_FILE")"
    (
        umask 077
        {
            printf 'WARDROBE_BENCH_NEO4J_USER=%s\n' "$NEO4J_USER"
            printf 'WARDROBE_BENCH_NEO4J_PASSWORD=%s\n' "$NEO4J_PASSWORD"
        } > "$CREDENTIALS_FILE"
    )
    echo "Credentials file: $CREDENTIALS_FILE"
}

require_docker

log_step "Preparing Neo4j benchmark container"
echo "Container name : $CONTAINER_NAME"
echo "Image          : $IMAGE"
echo "Bolt port      : 127.0.0.1:$HOST_BOLT_PORT"
echo "HTTP port      : 127.0.0.1:$HOST_HTTP_PORT"
echo "Database       : $DATABASE"
echo "Neo4j user     : $NEO4J_USER"
echo "Data volume    : $DATA_VOLUME"

if container_running; then
    echo "Neo4j benchmark container is already running: $CONTAINER_NAME"
elif container_exists; then
    log_step "Starting existing Neo4j container"
    docker start "$CONTAINER_NAME" >/dev/null
else
    log_step "Creating Neo4j container"
    echo "Docker may pull $IMAGE if it is not available locally."
    docker run -d \
        --name "$CONTAINER_NAME" \
        -e NEO4J_AUTH="$NEO4J_USER/$NEO4J_PASSWORD" \
        -e NEO4J_dbms_default__database="$DATABASE" \
        -p "127.0.0.1:$HOST_BOLT_PORT:7687" \
        -p "127.0.0.1:$HOST_HTTP_PORT:7474" \
        -v "$DATA_VOLUME:/data" \
        "$IMAGE"
fi

wait_for_neo4j
write_credentials_file

log_step "Ready"
cat <<EOF
Neo4j benchmark server is ready.

Benchmark flags:
  --targets neo4j --neo4j-uri 127.0.0.1:$HOST_BOLT_PORT --neo4j-database $DATABASE --neo4j-user $NEO4J_USER --neo4j-password-env WARDROBE_BENCH_NEO4J_PASSWORD

Default benchmark command:
  cargo run -p wardrobe-benchmark -- --targets neo4j

Shell environment:
  export WARDROBE_BENCH_NEO4J_USER="$NEO4J_USER"
  export WARDROBE_BENCH_NEO4J_PASSWORD="$NEO4J_PASSWORD"
EOF
