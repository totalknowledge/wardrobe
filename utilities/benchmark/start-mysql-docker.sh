#!/usr/bin/env bash
set -euo pipefail

CONTAINER_NAME="${WARDROBE_BENCH_MYSQL_CONTAINER:-wardrobe-benchmark-mysql}"
IMAGE="${WARDROBE_BENCH_MYSQL_IMAGE:-mysql:8.4}"
HOST_PORT="${WARDROBE_BENCH_MYSQL_PORT:-3306}"
DATABASE_EXPLICIT="${WARDROBE_BENCH_MYSQL_DATABASE+x}"
ROOT_PASSWORD_EXPLICIT="${WARDROBE_BENCH_MYSQL_ROOT_PASSWORD+x}"
BENCHMARK_USER_EXPLICIT="${WARDROBE_BENCH_MYSQL_USER+x}"
BENCHMARK_PASSWORD_EXPLICIT="${WARDROBE_BENCH_MYSQL_PASSWORD+x}"
DATABASE="${WARDROBE_BENCH_MYSQL_DATABASE:-wardrobe_benchmark}"
ROOT_PASSWORD="${WARDROBE_BENCH_MYSQL_ROOT_PASSWORD:-wardrobe_benchmark}"
BENCHMARK_USER="${WARDROBE_BENCH_MYSQL_USER:-wardrobe_benchmark}"
BENCHMARK_PASSWORD="${WARDROBE_BENCH_MYSQL_PASSWORD:-wardrobe_benchmark}"
DATA_VOLUME="${WARDROBE_BENCH_MYSQL_VOLUME:-wardrobe-benchmark-mysql-data-v2}"
INIT_SCHEMA="${WARDROBE_BENCH_INIT_SCHEMA:-0}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SCHEMA_FILE="$SCRIPT_DIR/library-schema.sql"
CREDENTIALS_FILE="${WARDROBE_BENCH_MYSQL_CREDENTIALS_FILE:-$WORKSPACE_ROOT/target/wardrobe-benchmark/mysql-credentials.env}"

log_step() {
    printf '\n==> %s\n' "$1"
}

require_docker() {
    log_step "Checking for Docker"
    if ! command -v docker >/dev/null 2>&1; then
        echo "docker is required to start the MySQL benchmark server" >&2
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

    log_step "Capturing credentials from existing MySQL container"
    local value
    if [ -z "$DATABASE_EXPLICIT" ]; then
        value="$(container_env_value MYSQL_DATABASE || true)"
        if [ -n "$value" ]; then
            DATABASE="$value"
        fi
    fi
    if [ -z "$ROOT_PASSWORD_EXPLICIT" ]; then
        value="$(container_env_value MYSQL_ROOT_PASSWORD || true)"
        if [ -n "$value" ]; then
            ROOT_PASSWORD="$value"
        fi
    fi
    if [ -z "$BENCHMARK_USER_EXPLICIT" ]; then
        value="$(container_env_value MYSQL_USER || true)"
        if [ -n "$value" ]; then
            BENCHMARK_USER="$value"
        fi
    fi
    if [ -z "$BENCHMARK_PASSWORD_EXPLICIT" ]; then
        value="$(container_env_value MYSQL_PASSWORD || true)"
        if [ -n "$value" ]; then
            BENCHMARK_PASSWORD="$value"
        fi
    fi
    echo "Credential source: Docker container environment with shell overrides when provided."
}

wait_for_mysql() {
    log_step "Waiting for MySQL to accept connections"
    echo "Container: $CONTAINER_NAME"
    echo "Probe: docker exec $CONTAINER_NAME mysql -u$BENCHMARK_USER -p*** $DATABASE -e SELECT 1"
    for attempt in $(seq 1 90); do
        if docker exec "$CONTAINER_NAME" mysql -u"$BENCHMARK_USER" -p"$BENCHMARK_PASSWORD" "$DATABASE" -e "SELECT 1" >/dev/null 2>&1 ||
            docker exec "$CONTAINER_NAME" mysql -uroot -p"$ROOT_PASSWORD" -e "SELECT 1" >/dev/null 2>&1; then
            echo "MySQL is accepting connections."
            return 0
        fi
        if [ $((attempt % 10)) -eq 0 ]; then
            echo "Still waiting for MySQL after $((attempt * 2)) seconds. Recent logs:"
            docker logs --tail 8 "$CONTAINER_NAME" 2>&1 || true
        else
            echo "Attempt $attempt/90: MySQL is not ready yet."
        fi
        sleep 2
    done
    echo "MySQL did not become ready in time. Inspect logs with: docker logs $CONTAINER_NAME" >&2
    exit 1
}

sql_identifier() {
    printf '%s' "$1" | sed 's/`/``/g'
}

sql_literal() {
    printf '%s' "$1" | sed "s/'/''/g"
}

ensure_benchmark_user() {
    log_step "Ensuring benchmark MySQL user"
    if docker exec "$CONTAINER_NAME" mysql -u"$BENCHMARK_USER" -p"$BENCHMARK_PASSWORD" "$DATABASE" -e "FLUSH TABLES" >/dev/null 2>&1; then
        echo "Benchmark user can connect and flush tables."
        return 0
    fi
    local database_identifier
    local user_literal
    local password_literal
    database_identifier="$(sql_identifier "$DATABASE")"
    user_literal="$(sql_literal "$BENCHMARK_USER")"
    password_literal="$(sql_literal "$BENCHMARK_PASSWORD")"
    if ! docker exec -i "$CONTAINER_NAME" mysql -uroot -p"$ROOT_PASSWORD" <<SQL
CREATE DATABASE IF NOT EXISTS \`$database_identifier\`;
CREATE USER IF NOT EXISTS '$user_literal'@'%' IDENTIFIED BY '$password_literal';
ALTER USER '$user_literal'@'%' IDENTIFIED BY '$password_literal';
GRANT ALL PRIVILEGES ON \`$database_identifier\`.* TO '$user_literal'@'%';
GRANT RELOAD ON *.* TO '$user_literal'@'%';
FLUSH PRIVILEGES;
SQL
    then
        echo "Could not authenticate as MySQL root inside $CONTAINER_NAME." >&2
        echo "If this container or volume was created before benchmark credentials were standardized, recreate the benchmark MySQL container or set WARDROBE_BENCH_MYSQL_ROOT_PASSWORD to the existing root password." >&2
        exit 1
    fi
}

write_credentials_file() {
    log_step "Writing benchmark credential fallback"
    mkdir -p "$(dirname "$CREDENTIALS_FILE")"
    (
        umask 077
        {
            printf 'WARDROBE_BENCH_MYSQL_USER=%s\n' "$BENCHMARK_USER"
            printf 'WARDROBE_BENCH_MYSQL_PASSWORD=%s\n' "$BENCHMARK_PASSWORD"
        } > "$CREDENTIALS_FILE"
    )
    echo "Credentials file: $CREDENTIALS_FILE"
}

require_docker
capture_existing_container_credentials

log_step "Preparing MySQL benchmark container"
echo "Container name : $CONTAINER_NAME"
echo "Image          : $IMAGE"
echo "Host port      : 127.0.0.1:$HOST_PORT"
echo "Database       : $DATABASE"
echo "Benchmark user : $BENCHMARK_USER"
echo "Data volume    : $DATA_VOLUME"
echo "Init schema    : $INIT_SCHEMA"

if container_running; then
    echo "MySQL benchmark container is already running: $CONTAINER_NAME"
elif container_exists; then
    log_step "Starting existing MySQL container"
    docker start "$CONTAINER_NAME" >/dev/null
else
    log_step "Creating MySQL container"
    echo "Docker may pull $IMAGE if it is not available locally."
    docker run -d \
        --name "$CONTAINER_NAME" \
        -e MYSQL_ROOT_PASSWORD="$ROOT_PASSWORD" \
        -e MYSQL_DATABASE="$DATABASE" \
        -e MYSQL_USER="$BENCHMARK_USER" \
        -e MYSQL_PASSWORD="$BENCHMARK_PASSWORD" \
        -p "127.0.0.1:$HOST_PORT:3306" \
        -v "$DATA_VOLUME:/var/lib/mysql" \
        "$IMAGE"
fi

wait_for_mysql
ensure_benchmark_user
write_credentials_file

if [ "$INIT_SCHEMA" = "1" ]; then
    log_step "Applying benchmark SQL schema"
    echo "Schema file: $SCHEMA_FILE"
    docker exec -i "$CONTAINER_NAME" mysql -uroot -p"$ROOT_PASSWORD" "$DATABASE" < "$SCHEMA_FILE"
    echo "Applied schema: $SCHEMA_FILE"
fi

log_step "Ready"
cat <<EOF
MySQL benchmark server is ready.

Benchmark flags:
  --targets mysql --mysql-host 127.0.0.1 --mysql-port $HOST_PORT --mysql-database $DATABASE --mysql-user $BENCHMARK_USER --mysql-password-env WARDROBE_BENCH_MYSQL_PASSWORD

Default benchmark command:
  cargo run -p wardrobe-benchmark -- --targets mysql

Shell environment:
  export WARDROBE_BENCH_MYSQL_USER="$BENCHMARK_USER"
  export WARDROBE_BENCH_MYSQL_PASSWORD="$BENCHMARK_PASSWORD"
EOF
