# Wardrobe Benchmark

`wardrobe-benchmark` runs the US-100 library transaction battery across a configured target matrix and prints a Markdown performance table.

The default run uses only embedded Wardrobe so it works without external database services:

```bash
cargo run -p wardrobe-benchmark
```

Run a quick smoke profile:

```bash
cargo run -p wardrobe-benchmark -- --entities 100 --books 500 --traversal-queries 10 --range-lookups 10
```

Progress messages are enabled by default and are written to stderr so the Markdown report remains clean on stdout or in `--output`. Pass `--quiet` or `--no-progress` to suppress them.

Run the full six-target matrix when the target drivers and services are available:

```bash
cargo run -p wardrobe-benchmark -- --targets all --output target/wardrobe-benchmark/report.md
```

Use `library-schema.sql` when you want to pre-create the SQLite or MySQL/MariaDB schema outside the harness. It mirrors the benchmark's entity/book tables and ISBN index while staying inside the SQL subset both engines accept.

## Docker Server Helpers

The benchmark directory includes helper scripts for starting the server-backed comparison targets in Docker:

```bash
bash utilities/benchmark/start-mysql-docker.sh
bash utilities/benchmark/start-mongodb-docker.sh
bash utilities/benchmark/start-neo4j-docker.sh
bash utilities/benchmark/start-wardrobe-docker.sh
```

Each script starts or reuses a named container, waits for readiness, writes any service credentials the benchmark needs under `target/wardrobe-benchmark`, and prints the matching benchmark flags. When a MySQL or Neo4j container already exists, the helper reads Docker's stored container environment first so the fallback credentials file matches the running service. The Wardrobe script runs the current workspace code inside the official Rust image, with named Docker volumes for storage, Cargo caches, and the Linux build target.

Useful examples:

```bash
WARDROBE_BENCH_INIT_SCHEMA=1 bash utilities/benchmark/start-mysql-docker.sh
cargo run -p wardrobe-benchmark -- --targets mysql
```

```bash
bash utilities/benchmark/start-mongodb-docker.sh
cargo run -p wardrobe-benchmark -- --targets mongodb --mongo-uri mongodb://127.0.0.1:27017
```

```bash
bash utilities/benchmark/start-neo4j-docker.sh
cargo run -p wardrobe-benchmark -- --targets neo4j
```

```bash
bash utilities/benchmark/start-wardrobe-docker.sh
cargo run -p wardrobe-benchmark -- --targets wardrobe-remote --wardrobe-remote-uri wardrobe://127.0.0.1:24842
```

Targets:

- `wardrobe-embedded` uses Wardrobe flat-file mode directly.
- `wardrobe-remote` uses a TCP Wardrobe server. If `--wardrobe-remote-uri` is omitted, the harness starts an in-process server on a loopback port for the run.
- `sqlite` uses an in-process persistent `rusqlite` connection backed by a WAL file under the run directory by default.
- `mongodb` uses a persistent native MongoDB Rust client against `--mongo-uri` and `--mongo-database`.
- `mysql` uses a persistent native MySQL Rust connection against `--mysql-host`, `--mysql-port`, and `--mysql-database`.
- `neo4j` uses a persistent native Neo4j Rust driver against `--neo4j-uri`, `--neo4j-database`, `--neo4j-user`, and `--neo4j-password-env`.

The MySQL helper creates a dedicated `wardrobe_benchmark` user and writes `target/wardrobe-benchmark/mysql-credentials.env`, which the benchmark reads by default. If neither the env vars nor the credentials file exists, the benchmark still sends the standard helper password `wardrobe_benchmark`. For custom credentials, set `WARDROBE_BENCH_MYSQL_USER` and `WARDROBE_BENCH_MYSQL_PASSWORD`, or pass `--mysql-user` and `--mysql-password-env <VAR>`. Use `--mysql-no-password` only for intentionally unauthenticated MySQL targets.

The Neo4j helper writes `target/wardrobe-benchmark/neo4j-credentials.env`, which the benchmark reads by default. If neither the env vars nor the credentials file exists, the benchmark still sends the standard helper password `wardrobe_benchmark`. For custom credentials, set `WARDROBE_BENCH_NEO4J_USER` and `WARDROBE_BENCH_NEO4J_PASSWORD`, or pass `--neo4j-user` and `--neo4j-password-env <VAR>`.

The benchmark phases are:

- Massive Ingestion: upsert entity and book records.
- Point Lookup: read deterministic random book records by primary id.
- Range Lookup: read deterministic random `quantity BETWEEN low AND high` ranges, fully materialize matches, and validate returned quantities.
- Complex Traversal: query books where author and editor criteria overlap using each engine's server-side traversal behavior, without benchmark-side N+1 follow-up fetches.
- Index Mutation: create, drop, and rebuild the book ISBN index.
- Delete by ID: delete deterministic random book records by primary id and verify they are gone.
- Targeted Purge: delete books matching a purge bucket filter using one server-side filter-delete command per target phase sample.
- Compaction: run the target's compact directive or closest native maintenance equivalent.

Neo4j semantic notes:

- Massive Ingestion uses `MERGE` for `Entity` and `Book` nodes and graph relationships (`AUTHORED_BY`, `EDITED_BY`).
- Complex Traversal uses a native graph-pattern match from entity node -> book -> entity node.
- Targeted Purge uses a single set-based `MATCH ... DETACH DELETE` statement.
- Compaction/flush uses `CALL db.checkpoint()` as the closest maintenance equivalent.

Graceful unavailable target behavior:

- If a configured target cannot be initialized or queried (for example Neo4j Docker is down), the benchmark continues with remaining targets.
- The report includes an `Unavailable` row for that target with diagnostics explaining the failure.

Current parity baseline:

- All targets use equivalent set-based traversal and purge operations for the same book relationship and purge-bucket predicates.
- Range Lookup uses the same deterministic numeric `quantity` bounds for every target. The default is 100 range lookups to keep full matrix runs usable; pass `--range-lookups <count>` for deeper historical samples.
- The benchmark currently avoids declaring additional non-unique application indexes (`author_id`, `editor_id`, `purge_bucket`) until Wardrobe exposes equivalent non-unique secondary index declarations.
- The Index Mutation phase exercises the shared ISBN index lifecycle (`books.isbn`) for each target.

The Markdown report includes operations per second, mean latency, p95 latency, p99 latency, and final storage footprint in bytes.
