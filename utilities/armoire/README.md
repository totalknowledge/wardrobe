# Armoire

Armoire is the Wardrobe database administration desktop utility. It lives under `utilities/armoire`, not `samples`, and its Angular frontend communicates with `wardrobe-core` through a Tauri Rust backend.

Current version: `0.26.723`.

## Capabilities

- Create, test, connect to, remember, rename, and remove Wardrobe source locations
- Connect to embedded paths or Wardrobe server URIs through `WardrobeClient`
- Discover wardrobes/databases, bays/schemas, and drawers using direct status arrays
- Create wardrobes, bays, and drawers
- Read drawer records and create records
- Persist saved connection metadata for the desktop application

## Development

Install frontend dependencies and start the Tauri development application from the repository root:

```sh
npm install --prefix utilities/armoire
npm run tauri --prefix utilities/armoire -- dev
```

To run only the Angular development server:

```sh
npm start --prefix utilities/armoire
```

The Angular server listens on `http://localhost:4200` by default.

## Build and Test

```sh
npm run build --prefix utilities/armoire
npm test --prefix utilities/armoire
cargo test -p armoire --all-targets
```

The frontend build writes to `utilities/armoire/dist/armoire/browser`, which is the Tauri `frontendDist`.

## Licensing

Armoire is licensed under the Armoire Source-Available Evaluation License (ASEL). Personal, hobby, educational, and internal commercial evaluation use is permitted under that license. Production deployment, embedding, revenue-generating use, or internal business use beyond evaluation requires a paid commercial license. See `utilities/armoire/LICENSE` for the authoritative terms.
