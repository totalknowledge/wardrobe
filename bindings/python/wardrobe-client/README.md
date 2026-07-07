# wardrobe-client

Pure Python client bindings for server-backed Wardrobe connections.

This package speaks the Wardrobe protocol directly over TCP or Unix socket connections. It does not load embedded native storage code.

## Usage

```python
from wardrobe_client import WardrobeClient

with WardrobeClient.open("wardrobe://localhost:24842") as client:
    result = client.status("Storage")
```

This package is not ready for PyPI publication yet.
