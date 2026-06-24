# CLI Script Sample

This folder contains `wardrobe-cli-demo.sh`, a shell script that walks the Wardrobe CLI through a library-themed workflow.

Run it from the repository root:

```bash
bash ./samples/cli-script/wardrobe-cli-demo.sh
```

To test against a running server instead of local embedded storage, pass a connection string:

```bash
bash ./samples/cli-script/wardrobe-cli-demo.sh wardrobe://localhost:24842
```

The default run uses `./samples/cli-script/data` as the embedded storage root and creates a `wardrobe` database with a `library` schema underneath it. Generated backup files live under `./samples/cli-script/backups`.

The sample keeps branch separation in the data itself by writing two `book` records with different `branch` values and quantities, while also linking each book to `author` and `editor` people records.

When the connection is remote, the script also exercises the `add user`, `grant permission`, and `revoke permission` commands.
