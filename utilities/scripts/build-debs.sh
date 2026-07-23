#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
OUTPUT_DIR="$REPO_ROOT/target/debian"

package_version() {
  sed -n 's/^version = "\([^"]*\)"$/\1/p' "$1/Cargo.toml" | head -n 1
}

build_package() {
  local package_name="$1"
  local crate_dir="$2"
  local binary_name="$3"
  local version
  local package_root

  version="$(package_version "$crate_dir")"
  if [[ -z "$version" ]]; then
    printf 'could not determine version for %s\n' "$package_name" >&2
    exit 1
  fi

  package_root="$STAGING_DIR/$package_name"
  mkdir -p "$package_root/DEBIAN" "$package_root/usr/bin" "$package_root/usr/share/doc/$package_name"

  sed \
    -e "s/@VERSION@/$version/g" \
    -e "s/@ARCHITECTURE@/$ARCHITECTURE/g" \
    "$crate_dir/packaging/control" > "$package_root/DEBIAN/control"
  install -m 755 "$REPO_ROOT/target/release/$binary_name" "$package_root/usr/bin/$binary_name"
  install -m 644 "$crate_dir/LICENSE" "$package_root/usr/share/doc/$package_name/copyright"
  dpkg-deb --build --root-owner-group "$package_root" "$OUTPUT_DIR/${package_name}_${version}_${ARCHITECTURE}.deb"
}

if ! command -v dpkg-deb >/dev/null; then
  printf 'dpkg-deb is required to build Debian packages\n' >&2
  exit 1
fi

mkdir -p "$OUTPUT_DIR"
STAGING_DIR="$(mktemp -d "$OUTPUT_DIR/.staging.XXXXXX")"
trap 'rm -rf "$STAGING_DIR"' EXIT
ARCHITECTURE="$(dpkg --print-architecture)"

cd "$REPO_ROOT"
cargo build --release --package wardrobe-cli --package wardrobe-server

build_package wardrobe-cli "$REPO_ROOT/cli" wardrobe
build_package wardrobe-server "$REPO_ROOT/server" wardrobe-server