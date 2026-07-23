# wardrobe-embedded

MIT-licensed native Python bindings for embedded Wardrobe storage. Current version: `0.26.723`; Python 3.10 or newer is required.

This package builds a Python extension with PyO3 and calls `wardrobe-core` directly. It does not use the C ABI.

## Usage

```python
from wardrobe_embedded import WardrobeEmbedded

root = WardrobeEmbedded.open("./wardrobe")
root.create({"Database": {"database_name": "publishing-house"}})
root.create(
    {"Schema": {"database_name": "publishing-house", "schema_name": "public"}}
)
root.create(
    {
        "Drawer": {
            "database_name": "publishing-house",
            "schema_name": "public",
            "drawer_name": "book",
        }
    }
)

engine = WardrobeEmbedded.open("./wardrobe/publishing-house/public")
engine.upsert({"_id": "book-01", "title": "The Lantern Index"}, "book")
records = engine.read("book")
databases = root.status("Databases")
```

Database, schema, and drawer status requests return lists directly.

This package is not ready for PyPI publication yet.
