#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_CONNECTION="$SCRIPT_DIR/data"
WARDROBE_NAME="publishing-house"
BAY_NAME="public-cli"
PERSON_DRAWER="person"
PUBLISHER_DRAWER="publisher"
BOOK_DRAWER="book"

PUBLISHER_PAYLOAD='{"_id":"pub_001","name":"Apex Press","founded_year":1994,"active":true}'
AUTHOR_PAYLOAD='{"_id":"author_001","name":"Elena Vance","role":"author","genres":["sci-fi","thriller"]}'
EDITOR_PAYLOAD='{"_id":"editor_001","name":"Marcus Sterling","role":"editor","department":"fiction"}'
BOOK_PAYLOAD='{"_id":"book_001","title":"The Quantum Horizon","publisher_id":"@publishing-house/public-cli/publisher:pub_001","author_id":"@publishing-house/public-cli/person:author_001","editor_id":"@publishing-house/public-cli/person:editor_001","page_count":420}'

usage() {
  cat <<'EOF'
Usage:
  wardrobe-cli-demo.sh [connection]
  wardrobe-cli-demo.sh --connection <connection>
  wardrobe-cli-demo.sh -c <connection>

When no connection is supplied, the script uses local embedded storage at ./samples/cli-script/data.
Pass a Wardrobe URI such as wardrobe://localhost:24842 to run the same workflow against a server.
EOF
}

section() {
  printf '\n== %s ==\n' "$1"
}

run_cli() {
  cargo run --quiet --manifest-path "$REPO_ROOT/Cargo.toml" -p wardrobe-cli -- "$CONNECTION" --pretty "$@"
}

CONNECTION="$DEFAULT_CONNECTION"
if [[ $# -gt 0 ]]; then
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    -c|--connection)
      if [[ $# -lt 2 ]]; then
        printf 'missing connection string after %s\n' "$1" >&2
        exit 1
      fi
      CONNECTION="$2"
      shift 2
      ;;
    --connection=*)
      CONNECTION="${1#*=}"
      shift
      ;;
    --)
      shift
      if [[ $# -gt 0 ]]; then
        CONNECTION="$1"
        shift
      fi
      ;;
    -*)
      printf 'unknown option: %s\n' "$1" >&2
      usage >&2
      exit 1
      ;;
    *)
      CONNECTION="$1"
      shift
      ;;
  esac
fi

if [[ $# -gt 0 ]]; then
  printf 'unexpected argument(s): %s\n' "$*" >&2
  exit 1
fi

REMOTE_MODE=false
case "$CONNECTION" in
  wardrobe://local/*|wardrobe+file://*|file://*)
    REMOTE_MODE=false
    ;;
  wardrobe://*|wardrobe+unix://*)
    REMOTE_MODE=true
    ;;
esac

if [[ "$REMOTE_MODE" == false ]]; then
  mkdir -p "$CONNECTION"
fi

printf 'Using connection: %s\n' "$CONNECTION"

section "Phase 1: Metadata & Inventory Discovery"
run_cli status tenants
run_cli status wardrobes

run_cli create wardrobe "$WARDROBE_NAME"
run_cli create bay "$WARDROBE_NAME/$BAY_NAME"
run_cli create drawer "$WARDROBE_NAME/$BAY_NAME/$PUBLISHER_DRAWER"
run_cli create drawer "$WARDROBE_NAME/$BAY_NAME/$PERSON_DRAWER"
run_cli create drawer "$WARDROBE_NAME/$BAY_NAME/$BOOK_DRAWER"

printf "System Wardrobes:\n"
run_cli status wardrobes
printf "Available Bays in '%s':\n" "$WARDROBE_NAME"
run_cli status bays "$WARDROBE_NAME"
printf "Drawers in %s/%s:\n" "$WARDROBE_NAME" "$BAY_NAME"
run_cli status drawers "$WARDROBE_NAME/$BAY_NAME"

section "Phase 2: Relational Data Population"
run_cli upsert "$WARDROBE_NAME/$BAY_NAME/$PUBLISHER_DRAWER" "$PUBLISHER_PAYLOAD"
printf 'Persisted publisher -> @%s/%s/%s:pub_001\n' "$WARDROBE_NAME" "$BAY_NAME" "$PUBLISHER_DRAWER"
run_cli upsert "$WARDROBE_NAME/$BAY_NAME/$PERSON_DRAWER" "$AUTHOR_PAYLOAD"
printf 'Persisted author (in person drawer) -> @%s/%s/%s:author_001\n' "$WARDROBE_NAME" "$BAY_NAME" "$PERSON_DRAWER"
run_cli upsert "$WARDROBE_NAME/$BAY_NAME/$PERSON_DRAWER" "$EDITOR_PAYLOAD"
printf 'Persisted editor (in person drawer) -> @%s/%s/%s:editor_001\n' "$WARDROBE_NAME" "$BAY_NAME" "$PERSON_DRAWER"
run_cli upsert "$WARDROBE_NAME/$BAY_NAME/$BOOK_DRAWER" "$BOOK_PAYLOAD"
printf 'Persisted book -> @%s/%s/%s:book_001\n' "$WARDROBE_NAME" "$BAY_NAME" "$BOOK_DRAWER"

section "Phase 3: Filter Query Execution"
run_cli read "$WARDROBE_NAME/$BAY_NAME/$PERSON_DRAWER" '{"role":"author"}' '{"order_by":"name","order_direction":"asc","offset":0,"limit":10}'

section "Phase 4: Relation Verification"
run_cli read '@publishing-house/public-cli/book:book_001'
run_cli read '@publishing-house/public-cli/person:author_001'
run_cli read '@publishing-house/public-cli/person:editor_001'

section "Phase 5: Maintenance & Stress Test Cycle"
run_cli count "$WARDROBE_NAME/$BAY_NAME/$PERSON_DRAWER"
run_cli count "$WARDROBE_NAME/$BAY_NAME/$BOOK_DRAWER"
printf 'Maintenance check above: personnel count then book count\n'

for i in 0 1 2 3 4; do
  run_cli upsert "$WARDROBE_NAME/$BAY_NAME/$BOOK_DRAWER" "{\"_id\":\"temp_book_${i}\",\"title\":\"Temporary Draft\",\"page_count\":100}"
  run_cli delete "$WARDROBE_NAME/$BAY_NAME/$BOOK_DRAWER" "{\"_id\":\"temp_book_${i}\"}"
done
printf 'Stress test cycle completed (5 temporary book upserts/deletes).\n'

section "Phase 6: Detailed Engine Inspection"
run_cli status wardrobes
run_cli status bays "$WARDROBE_NAME"
run_cli status drawers "$WARDROBE_NAME/$BAY_NAME"
run_cli count "$WARDROBE_NAME/$BAY_NAME/$PUBLISHER_DRAWER"
run_cli count "$WARDROBE_NAME/$BAY_NAME/$PERSON_DRAWER"
run_cli count "$WARDROBE_NAME/$BAY_NAME/$BOOK_DRAWER"

section "Phase 7: Final State Reconciliation & Integrity"
run_cli read "$WARDROBE_NAME/$BAY_NAME/$PERSON_DRAWER"
run_cli read "$WARDROBE_NAME/$BAY_NAME/$BOOK_DRAWER"
run_cli read '@publishing-house/public-cli/publisher:pub_001'

printf '\nPublishing domain integration test suite executed successfully. All 7 phases completed.\n'
