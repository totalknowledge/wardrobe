#!/usr/bin/env bash
set -euo pipefail

CONTAINER_NAME="${WARDROBE_BENCH_MONGO_CONTAINER:-wardrobe-benchmark-mongodb}"
IMAGE="${WARDROBE_BENCH_MONGO_IMAGE:-mongo:7}"
HOST_PORT="${WARDROBE_BENCH_MONGO_PORT:-27017}"
DATABASE="${WARDROBE_BENCH_MONGO_DATABASE:-wardrobe_benchmark}"
DATA_VOLUME="${WARDROBE_BENCH_MONGO_VOLUME:-wardrobe-benchmark-mongodb-data}"

log_step() {
    printf '\n==> %s\n' "$1"
}

require_docker() {
    log_step "Checking for Docker"
    if ! command -v docker >/dev/null 2>&1; then
        echo "docker is required to start the MongoDB benchmark server" >&2
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

wait_for_mongodb() {
    log_step "Waiting for MongoDB to accept connections"
    echo "Container: $CONTAINER_NAME"
    echo "Probe: docker exec $CONTAINER_NAME mongosh --quiet --eval 'db.adminCommand({ ping: 1 })'"
    for attempt in $(seq 1 90); do
        if docker exec "$CONTAINER_NAME" mongosh --quiet --eval 'db.adminCommand({ ping: 1 })' >/dev/null 2>&1; then
            echo "MongoDB is accepting connections."
            return 0
        fi
        if [ $((attempt % 10)) -eq 0 ]; then
            echo "Still waiting for MongoDB after $((attempt * 2)) seconds. Recent logs:"
            docker logs --tail 8 "$CONTAINER_NAME" 2>&1 || true
        else
            echo "Attempt $attempt/90: MongoDB is not ready yet."
        fi
        sleep 2
    done
    echo "MongoDB did not become ready in time. Inspect logs with: docker logs $CONTAINER_NAME" >&2
    exit 1
}

require_docker

log_step "Preparing MongoDB benchmark container"
echo "Container name : $CONTAINER_NAME"
echo "Image          : $IMAGE"
echo "Host port      : 127.0.0.1:$HOST_PORT"
echo "Database       : $DATABASE"
echo "Data volume    : $DATA_VOLUME"

if container_running; then
    echo "MongoDB benchmark container is already running: $CONTAINER_NAME"
elif container_exists; then
    log_step "Starting existing MongoDB container"
    docker start "$CONTAINER_NAME" >/dev/null
else
    log_step "Creating MongoDB container"
    echo "Docker may pull $IMAGE if it is not available locally."
    docker run -d \
        --name "$CONTAINER_NAME" \
        -p "127.0.0.1:$HOST_PORT:27017" \
        -v "$DATA_VOLUME:/data/db" \
        "$IMAGE"
fi

wait_for_mongodb

log_step "Ready"
cat <<EOF
MongoDB benchmark server is ready.

Benchmark flags:
  --targets mongodb --mongo-uri mongodb://127.0.0.1:$HOST_PORT --mongo-database $DATABASE
EOF
