# wardrobe-client

MIT-licensed pure Python client bindings for server-backed Wardrobe connections. Current version: `0.26.723`; Python 3.10 or newer is required.

This package speaks the Wardrobe protocol directly over TCP or Unix socket connections. It does not load embedded native storage code.

## Usage

```python
from wardrobe_client import WardrobeClient

with WardrobeClient.open("wardrobe://localhost:24842") as client:
    databases = client.status("Databases")
    schemas = client.status({"Schemas": {"database_name": "publishing-house"}})
    drawers = client.status(
        {
            "Drawers": {
                "database_name": "publishing-house",
                "schema_name": "public",
            }
        }
    )
    records = client.read(["book", {"page_count": 420}])
```

Database, schema, and drawer status requests return lists directly.

This package is not ready for PyPI publication yet.
