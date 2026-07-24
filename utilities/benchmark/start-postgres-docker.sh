#!/usr/bin/env bash
set -euo pipefail

CONTAINER_NAME="${WARDROBE_BENCH_POSTGRES_CONTAINER:-wardrobe-benchmark-postgres}"
IMAGE="${WARDROBE_BENCH_POSTGRES_IMAGE:-postgres:17}"
HOST_PORT="${WARDROBE_BENCH_POSTGRES_PORT:-5432}"
DATABASE_EXPLICIT="${WARDROBE_BENCH_POSTGRES_DATABASE+x}"
BENCHMARK_USER_EXPLICIT="${WARDROBE_BENCH_POSTGRES_USER+x}"
BENCHMARK_PASSWORD_EXPLICIT="${WARDROBE_BENCH_POSTGRES_PASSWORD+x}"
DATABASE="${WARDROBE_BENCH_POSTGRES_DATABASE:-wardrobe_benchmark}"
BENCHMARK_USER="${WARDROBE_BENCH_POSTGRES_USER:-wardrobe_benchmark}"
BENCHMARK_PASSWORD="${WARDROBE_BENCH_POSTGRES_PASSWORD:-wardrobe_benchmark}"
DATA_VOLUME="${WARDROBE_BENCH_POSTGRES_VOLUME:-wardrobe-benchmark-postgres-data}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CREDENTIALS_FILE="${WARDROBE_BENCH_POSTGRES_CREDENTIALS_FILE:-$WORKSPACE_ROOT/target/wardrobe-benchmark/postgres-credentials.env}"

log_step() {
    printf '\n==> %s\n' "$1"
}

require_docker() {
    log_step "Checking for Docker"
    if ! command -v docker >/dev/null 2>&1; then
        echo "docker is required to start the PostgreSQL benchmark server" >&2
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

container_env_value() {
    docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$CONTAINER_NAME" 2>/dev/null |
        sed -n "s/^$1=//p" |
        head -n 1
}

capture_existing_container_credentials() {
    if ! container_exists; then
        return 0
    fi

    log_step "Capturing credentials from existing PostgreSQL container"
    local value
    if [ -z "$DATABASE_EXPLICIT" ]; then
        value="$(container_env_value POSTGRES_DB || true)"
        if [ -n "$value" ]; then
            DATABASE="$value"
        fi
    fi
    if [ -z "$BENCHMARK_USER_EXPLICIT" ]; then
        value="$(container_env_value POSTGRES_USER || true)"
        if [ -n "$value" ]; then
            BENCHMARK_USER="$value"
        fi
    fi
    if [ -z "$BENCHMARK_PASSWORD_EXPLICIT" ]; then
        value="$(container_env_value POSTGRES_PASSWORD || true)"
        if [ -n "$value" ]; then
            BENCHMARK_PASSWORD="$value"
        fi
    fi
    echo "Credential source: Docker container environment with shell overrides when provided."
}

wait_for_postgres() {
    log_step "Waiting for PostgreSQL to accept connections"
    for attempt in $(seq 1 90); do
        if docker exec "$CONTAINER_NAME" pg_isready -U "$BENCHMARK_USER" -d "$DATABASE" >/dev/null 2>&1; then
            echo "PostgreSQL is accepting connections."
            return 0
        fi
        if [ $((attempt % 10)) -eq 0 ]; then
            echo "Still waiting for PostgreSQL after $((attempt * 2)) seconds. Recent logs:"
            docker logs --tail 8 "$CONTAINER_NAME" 2>&1 || true
        else
            echo "Attempt $attempt/90: PostgreSQL is not ready yet."
        fi
        sleep 2
    done
    echo "PostgreSQL did not become ready in time. Inspect logs with: docker logs $CONTAINER_NAME" >&2
    exit 1
}

write_credentials_file() {
    log_step "Writing benchmark credential fallback"
    mkdir -p "$(dirname "$CREDENTIALS_FILE")"
    (
        umask 077
        {
            printf 'WARDROBE_BENCH_POSTGRES_USER=%s\n' "$BENCHMARK_USER"
            printf 'WARDROBE_BENCH_POSTGRES_PASSWORD=%s\n' "$BENCHMARK_PASSWORD"
        } > "$CREDENTIALS_FILE"
    )
    echo "Credentials file: $CREDENTIALS_FILE"
}

require_docker
capture_existing_container_credentials

log_step "Preparing PostgreSQL benchmark container"
echo "Container name : $CONTAINER_NAME"
echo "Image          : $IMAGE"
echo "Host port      : 127.0.0.1:$HOST_PORT"
echo "Database       : $DATABASE"
echo "Benchmark user : $BENCHMARK_USER"
echo "Data volume    : $DATA_VOLUME"

if container_running; then
    echo "PostgreSQL benchmark container is already running: $CONTAINER_NAME"
elif container_exists; then
    log_step "Starting existing PostgreSQL container"
    docker start "$CONTAINER_NAME" >/dev/null
else
    log_step "Creating PostgreSQL container"
    docker run -d \
        --name "$CONTAINER_NAME" \
        -e POSTGRES_DB="$DATABASE" \
        -e POSTGRES_USER="$BENCHMARK_USER" \
        -e POSTGRES_PASSWORD="$BENCHMARK_PASSWORD" \
        -p "127.0.0.1:$HOST_PORT:5432" \
        -v "$DATA_VOLUME:/var/lib/postgresql/data" \
        "$IMAGE"
fi

wait_for_postgres
write_credentials_file

log_step "Ready"
cat <<EOF
PostgreSQL benchmark server is ready.

Benchmark flags:
  --targets postgres --postgres-host 127.0.0.1 --postgres-port $HOST_PORT --postgres-database $DATABASE --postgres-user $BENCHMARK_USER --postgres-password-env WARDROBE_BENCH_POSTGRES_PASSWORD

Default benchmark command:
  cargo run -p wardrobe-benchmark -- --targets postgres

Shell environment:
  export WARDROBE_BENCH_POSTGRES_USER="$BENCHMARK_USER"
  export WARDROBE_BENCH_POSTGRES_PASSWORD="$BENCHMARK_PASSWORD"
EOF