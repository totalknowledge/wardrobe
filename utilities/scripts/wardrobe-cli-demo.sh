#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_CONNECTION="$SCRIPT_DIR/data"
BACKUP_DIR="$SCRIPT_DIR/backups"
BACKUP_FILE="$BACKUP_DIR/library-backup.wrb"

AUTHOR_PAYLOAD='{"_id":"author-mara","name":"Mara Stone","role":"author"}'
EDITOR_PAYLOAD='{"_id":"editor-oren","name":"Oren Vale","role":"editor"}'
BOOK_DOWNTOWN_PAYLOAD='{"_id":"book-lantern-downtown","title":"The Lantern Index","branch":"downtown","quantity":4,"author_id":"@wardrobe/library/people:author-mara","editor_id":"@wardrobe/library/people:editor-oren"}'
BOOK_UPTOWN_PAYLOAD='{"_id":"book-lantern-uptown","title":"The Lantern Index","branch":"uptown","quantity":2,"author_id":"@wardrobe/library/people:author-mara","editor_id":"@wardrobe/library/people:editor-oren"}'
BOOK_DRAFT_PAYLOAD='{"_id":"book-draft","title":"Temporary Draft","branch":"downtown","quantity":1,"author_id":"@wardrobe/library/people:author-mara","editor_id":"@wardrobe/library/people:editor-oren"}'
ADMIN_PAYLOAD='{"username":"branch_admin","role":"operator","display_name":"Branch Admin"}'

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
  cargo run --quiet --manifest-path "$REPO_ROOT/Cargo.toml" -p wardrobe-cli -- --target "$CONNECTION" --pretty "$@"
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
mkdir -p "$BACKUP_DIR"

printf 'Using connection: %s\n' "$CONNECTION"

section "Discovery before setup"
run_cli status tenants
run_cli status wardrobes

section "Create the wardrobe library"
run_cli create wardrobe wardrobe
run_cli create bay wardrobe/library
run_cli create drawer wardrobe/library/people
run_cli create drawer wardrobe/library/book

section "Verify structure"
run_cli status wardrobes
run_cli status bays wardrobe
run_cli status drawers wardrobe/library
run_cli status path wardrobe/library/book

section "Schema and relationship setup"
run_cli alter index wardrobe/library/book title
run_cli drop index wardrobe/library/book title
run_cli alter relationship wardrobe/library/book author_id wardrobe/library/people M:1
run_cli alter relationship wardrobe/library/book editor_id wardrobe/library/people M:1

section "People and branch listings"
run_cli upsert wardrobe/library/people "$AUTHOR_PAYLOAD"
run_cli upsert wardrobe/library/people "$EDITOR_PAYLOAD"
run_cli upsert wardrobe/library/book "$BOOK_DOWNTOWN_PAYLOAD"
run_cli upsert wardrobe/library/book "$BOOK_UPTOWN_PAYLOAD"
run_cli upsert wardrobe/library/book "$BOOK_DRAFT_PAYLOAD"
run_cli count wardrobe/library/book '{"branch":"downtown"}'
run_cli count wardrobe/library/book '{"branch":"uptown"}'
run_cli read wardrobe/library/book '{"branch":"downtown"}'
run_cli read wardrobe/library/book '{"branch":"uptown"}'
run_cli inspect wardrobe/library/book
run_cli delete wardrobe/library/book '{"_id":"book-draft"}'
run_cli count wardrobe/library/book

section "Maintenance"
run_cli compact wardrobe/library

section "Backup and restore"
run_cli backup wardrobe/library "$BACKUP_FILE"
run_cli restore wardrobe/library-archive "$BACKUP_FILE"
run_cli status bays wardrobe
run_cli status drawers wardrobe/library-archive
run_cli count wardrobe/library-archive/book '{"branch":"downtown"}'
run_cli count wardrobe/library-archive/book '{"branch":"uptown"}'
run_cli read wardrobe/library-archive/book '{"branch":"downtown"}'
run_cli read wardrobe/library-archive/book '{"branch":"uptown"}'

section "User administration"
run_cli create user "$ADMIN_PAYLOAD"
run_cli grant permission branch_admin wardrobe/library:rud
run_cli revoke permission branch_admin wardrobe/library:d
