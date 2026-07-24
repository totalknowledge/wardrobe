# wardrobe-client

MIT-licensed pure Python client bindings for server-backed Wardrobe connections. Current version: `0.26.725`; Python 3.10 or newer is required.

This package speaks the Wardrobe protocol directly over TCP or Unix socket connections. It does not load embedded native storage code.

## Usage

```python
from wardrobe_client import WardrobeClient, relationship_request

with WardrobeClient.open("wardrobe://localhost:24842") as client:
    client.alter(
        relationship_request(
            "publishing-house/public/character",
            "item_map",
            "publishing-house/public/item",
        )
    )
    client.upsert(
        {
            "_id": "hero",
            "attributes": {"strength": 18, "proficiencies": ["athletics"]},
        },
        "publishing-house/public/character",
    )
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
