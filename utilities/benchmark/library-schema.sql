-- Library Schema Matrix for wardrobe-benchmark.
--
-- This file is intentionally limited to SQL accepted by both SQLite and
-- MySQL/MariaDB. Select or create the target database outside this script.
--
-- SQLite:
--   sqlite3 target/wardrobe-benchmark/library.sqlite < utilities/benchmark/library-schema.sql
--
-- MySQL/MariaDB:
--   mysql --database wardrobe_benchmark < utilities/benchmark/library-schema.sql
--
-- Optional SQLite session setup:
--   PRAGMA foreign_keys = ON;
--   PRAGMA journal_mode = WAL;
--   PRAGMA synchronous = FULL;

DROP TABLE IF EXISTS books;
DROP TABLE IF EXISTS entities;

CREATE TABLE entities (
    id VARCHAR(64) PRIMARY KEY,
    display_name VARCHAR(255) NOT NULL,
    role VARCHAR(32) NOT NULL,
    cohort INTEGER NOT NULL
);

CREATE TABLE books (
    id VARCHAR(64) PRIMARY KEY,
    isbn VARCHAR(64) NOT NULL,
    title VARCHAR(255) NOT NULL,
    author_id VARCHAR(64) NOT NULL,
    editor_id VARCHAR(64) NOT NULL,
    branch VARCHAR(32) NOT NULL,
    quantity INTEGER NOT NULL,
    purge_bucket INTEGER NOT NULL,
    CONSTRAINT fk_books_author FOREIGN KEY (author_id) REFERENCES entities(id),
    CONSTRAINT fk_books_editor FOREIGN KEY (editor_id) REFERENCES entities(id)
);

CREATE INDEX idx_books_isbn ON books(isbn);
