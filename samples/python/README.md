# Wardrobe Python Sample

This MIT-licensed version `0.26.724` sample uses the native `wardrobe-embedded` binding to run the
publishing-house workflow from the Rust basic-usage sample. It creates
publisher, person, and book drawers, stores related publishing records, queries
authors, verifies relationships, and runs a temporary-record cleanup cycle in
the `public_py` bay.

From the repository root, build the local native binding and run the sample:

```sh
cargo build --release --manifest-path bindings/python/wardrobe-embedded/Cargo.toml
python3 samples/python/main.py
```

The sample uses an installed `wardrobe-embedded` package when available and
otherwise loads the repository's local release or debug build directly.

The sample stores its files under the ignored repository-root
`./wardrobe/publishing-house/public_py` directory.

Database, bay, and drawer status calls return Python lists directly; the sample does not unwrap result-side variant objects.
