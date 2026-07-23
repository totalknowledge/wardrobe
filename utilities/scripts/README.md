# CLI Script Sample

This folder contains the version `0.26.723` `wardrobe-cli-demo.sh` workflow, which walks the current positional-target Wardrobe CLI through a library-themed scenario.

Run it from the repository root:

```bash
bash ./utilities/scripts/wardrobe-cli-demo.sh
```

To test against a running server instead of local embedded storage, pass a connection string:

```bash
bash ./utilities/scripts/wardrobe-cli-demo.sh wardrobe://localhost:24842
```

The default run uses `./utilities/scripts/data` as the embedded storage root and creates a `wardrobe` database with a `library` schema underneath it. Generated backup files live under `./utilities/scripts/backups`.

The sample keeps branch separation in the data itself by writing two `book` records with different `branch` values and quantities, while also linking each book to `author` and `editor` people records.

When the connection is remote, the script also exercises the `create user`, `grant permission`, and `revoke permission` commands.
