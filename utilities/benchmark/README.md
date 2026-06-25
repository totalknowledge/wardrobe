# Wardrobe Benchmark

`wardrobe-benchmark` runs the US-100 library transaction battery across a configured target matrix and prints a Markdown performance table.

The default run uses only embedded Wardrobe so it works without external database services:

```bash
cargo run -p wardrobe-benchmark
```

Run a quick smoke profile:

```bash
cargo run -p wardrobe-benchmark -- --entities 100 --books 500 --traversal-queries 10
```

Progress messages are enabled by default and are written to stderr so the Markdown report remains clean on stdout or in `--output`. Pass `--quiet` or `--no-progress` to suppress them.

Run the full five-target matrix when the target drivers and services are available:

```bash
cargo run -p wardrobe-benchmark -- --targets all --output target/wardrobe-benchmark/report.md
```

Use `library-schema.sql` when you want to pre-create the SQLite or MySQL/MariaDB schema outside the harness. It mirrors the benchmark's entity/book tables and ISBN index while staying inside the SQL subset both engines accept.

## Docker Server Helpers

The benchmark directory includes helper scripts for starting the server-backed comparison targets in Docker:

```bash
bash utilities/benchmark/start-mysql-docker.sh
bash utilities/benchmark/start-mongodb-docker.sh
bash utilities/benchmark/start-wardrobe-docker.sh
```

Each script starts or reuses a named container, waits for readiness, and prints the matching benchmark flags. The Wardrobe script runs the current workspace code inside the official Rust image, with named Docker volumes for storage, Cargo caches, and the Linux build target.

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
bash utilities/benchmark/start-wardrobe-docker.sh
cargo run -p wardrobe-benchmark -- --targets wardrobe-remote --wardrobe-remote-uri wardrobe://127.0.0.1:24842
```

Targets:

- `wardrobe-embedded` uses Wardrobe flat-file mode directly.
- `wardrobe-remote` uses a TCP Wardrobe server. If `--wardrobe-remote-uri` is omitted, the harness starts an in-process server on a loopback port for the run.
- `sqlite` uses an in-process persistent `rusqlite` connection backed by a WAL file under the run directory by default.
- `mongodb` uses a persistent native MongoDB Rust client against `--mongo-uri` and `--mongo-database`.
- `mysql` uses a persistent native MySQL Rust connection against `--mysql-host`, `--mysql-port`, and `--mysql-database`.

The MySQL helper creates a dedicated `wardrobe_benchmark` user and writes `target/wardrobe-benchmark/mysql-credentials.env`, which the benchmark reads by default. For custom credentials, set `WARDROBE_BENCH_MYSQL_USER` and `WARDROBE_BENCH_MYSQL_PASSWORD`, or pass `--mysql-user` and `--mysql-password-env <VAR>`.

The benchmark phases are:

- Massive Ingestion: upsert entity and book records.
- Index Mutation: create, drop, and rebuild the book ISBN index.
- Complex Traversal: query books where author and editor criteria overlap, then materialize author and editor details into each returned book.
- Targeted Purge: delete books matching a purge bucket filter.
- Compaction: run the target's clean/vacuum/compact directive.

The Markdown report includes operations per second, mean latency, p95 latency, p99 latency, and final storage footprint in bytes.
