# Utility Scripts

This folder contains utility scripts for demoing, building, packaging, and publishing Wardrobe workspace components (release `0.26.725`).

## Scripts

### 1. `publish-pypi.sh`
Builds and publishes pure Python client (`wardrobe-client`) and native embedded extension (`wardrobe-embedded`) packages to PyPI.

```bash
# Dry-run build and verification:
./utilities/scripts/publish-pypi.sh --dry-run

# Publish to PyPI using token:
./utilities/scripts/publish-pypi.sh --token "$PYPI_TOKEN"

# Publish to TestPyPI:
./utilities/scripts/publish-pypi.sh --repository testpypi --token "$TEST_PYPI_TOKEN"
```

### 2. `publish-crates.sh`
Publishes `wardrobe-client` and `wardrobe-embedded` to `crates.io` in the required dependency sequence.

```bash
# Dry-run publish verification:
./utilities/scripts/publish-crates.sh --dry-run --allow-dirty

# Publish to crates.io:
./utilities/scripts/publish-crates.sh --token "$CARGO_TOKEN"
```

### 3. `publish-containers.sh`
Builds and pushes multi-architecture Docker container images for `wardrobe-server`.

```bash
./utilities/scripts/publish-containers.sh --tag 0.26.725
```

### 4. `build-debs.sh`
Builds Debian/Ubuntu `.deb` packages for `wardrobe-cli` and `wardrobe-server`.

```bash
./utilities/scripts/build-debs.sh
```

### 5. `start-server.sh`
Launches a local `wardrobe-server` daemon instance.

```bash
./utilities/scripts/start-server.sh
```

### 6. `wardrobe-cli-demo.sh`
Interactive CLI workflow demo demonstrating database, schema, drawer, and record operations.

```bash
./utilities/scripts/wardrobe-cli-demo.sh
```
