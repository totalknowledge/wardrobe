#!/usr/bin/env bash
set -e

SERVER_BIN="./target/release/wardrobe-server"
STORAGE_ROOT="./wardrobe"
SERVER_PORT="24842"
SERVER_HOST="127.0.0.1"

if [ ! -f "$SERVER_BIN" ]; then
    echo "=== Building Wardrobe Server in Release Mode ==="
    cargo build --release --bin wardrobe-server
fi

if [ ! -d "$STORAGE_ROOT" ]; then
    echo "=== Initializing Storage Root Directory at $STORAGE_ROOT ==="
    mkdir -p "$STORAGE_ROOT"
fi

if command -v nproc &>/dev/null; then
    CPU_CORES=$(nproc)
elif command -v sysctl &>/dev/null; then
    CPU_CORES=$(sysctl -n hw.ncpu)
else
    CPU_CORES=4
fi

CONNECTION_POOL_LIMIT=$(( (CPU_CORES + 1) / 2 ))

echo "========================================================================="
echo " STARTING WARDROBE DATABASE SERVER"
echo "========================================================================="
echo " Storage Root   : $(cd "$STORAGE_ROOT" && pwd)"
echo " Listen Address  : wardrobe://$SERVER_HOST:$SERVER_PORT"
echo " CPU Cores Found : $CPU_CORES"
echo " Connection Pool : $CONNECTION_POOL_LIMIT active workers (0.5 * Cores, rounded up)"
echo "========================================================================="
echo ""

exec "$SERVER_BIN" \
    --data-dir "$STORAGE_ROOT" \
    --tcp-bind "$SERVER_HOST:$SERVER_PORT" \
    --connection-pool-limit "$CONNECTION_POOL_LIMIT"
