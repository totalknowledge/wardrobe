# Wardrobe JavaScript Sample

This MIT-licensed version `0.26.724` sample uses `@wardrobe/embedded` to run the publishing-house workflow from
the Rust basic-usage sample. It creates publisher, person, and book drawers,
stores related publishing records, queries authors, verifies relationships, and
runs a temporary-record cleanup cycle in the `public_js` bay.

From the repository root, run:

```sh
cargo build --release -p wardrobe-js-ts
npm install --prefix samples/javascript
npm start --prefix samples/javascript
```

The `file:` dependency installs the locally built `@wardrobe/embedded` binding
directly. The sample stores its files under the ignored repository-root
`./wardrobe/publishing-house/public_js` directory.

Database, bay, and drawer discovery use the binding's direct status arrays; no `Databases`, `Schemas`, or `Drawers` response helper is required.
