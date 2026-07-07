# wardrobe-embedded

Native Python bindings for embedded Wardrobe storage.

This package builds a Python extension with PyO3 and calls `wardrobe-core` directly. It does not use the C ABI.

## Usage

```python
from wardrobe_embedded import WardrobeEmbedded

engine = WardrobeEmbedded.open("./wardrobe-data")
result = engine.status("Storage")
```

This package is not ready for PyPI publication yet.
