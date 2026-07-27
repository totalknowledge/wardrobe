# wardrobe-embedded

`wardrobe-embedded` is the in-process Wardrobe storage engine for Rust applications. It opens a local data directory directly and provides the same command model used by `wardrobe-server`.

Use [`wardrobe-client`](https://crates.io/crates/wardrobe-client) when the application only needs to connect to a running Wardrobe server and should not include the embedded storage engine.

## Installation

```text
cargo add wardrobe-embedded serde_json
```

## Quick Start

```rust
use serde_json::json;
use wardrobe_embedded::{
    OperationFilter, OperationOptions, ReadResult, WardrobeEngine,
};

fn main() -> std::io::Result<()> {
    let engine = WardrobeEngine::open("./wardrobe-data")?;

    let stored = engine.upsert(
        json!({
            "_id": "book-01",
            "title": "The Lantern Index"
        }),
        OperationFilter::drawer("book"),
        OperationOptions::default(),
    )?;

    println!("stored: {:?}", stored.into_pointers());

    if let ReadResult::Records(books) = engine.read(
        OperationFilter::drawer("book"),
        OperationOptions::default(),
    )? {
        println!("books: {}", books.len());
    }

    Ok(())
}
```

`WardrobeEngine` is the embedded entry point. Network and Unix-socket connections are provided separately by `wardrobe-client`.

Do not open the same data directory through an embedded process while `wardrobe-server` owns it.

## Operations

The embedded API exposes the canonical `read`, `upsert`, `delete`, `inspect`, `count`, `compact`, `create`, `alter`, `drop`, `backup`, `restore`, `grant`, `revoke`, and `status` operations.

API documentation is available on [docs.rs](https://docs.rs/wardrobe-embedded).

## License

MIT
