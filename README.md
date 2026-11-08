<h1 align="center">sqliters Source Repository</h1>

<p align="center"><strong>English</strong> | <a href="README.tr.md">Türkçe</a></p>

This repository contains the complete Rust workspace source code and adapted
test suite for **sqliters**, an independent, zero-external-dependency Rust
port modeled on the [SQLite database engine](https://sqlite.org/) (reference
baseline: SQLite 3.54.1).

See the official [SQLite documentation](https://sqlite.org/) for general
background on SQLite's SQL dialect and file format.  This README describes
the `sqliters` source tree, its experimental status, licensing, and how its
Rust crates map to the architecture of SQLite.

> [!WARNING]
> **Experimental Project — No Warranty for Production Use**
>
> `sqliters` is an **experimental** engineering and research project.  It is
> intended for experimentation, prototyping, testing, and studying database
> internals in Rust.  **No guarantee of data safety, crash durability,
> completeness, or fitness for serious, critical, or production workloads is
> provided.**  For production or mission-critical applications, use the
> official [SQLite](https://sqlite.org/) library.

## Independent Project Notice

`sqliters` is an independent reimplementation in Rust inspired by SQLite's
public-domain source code, file format specification, and test suite.  It is
**not** an official SQLite product and is not affiliated with or endorsed by
the SQLite developers or Hipp, Wyrick & Company, Inc.

## Public Domain & Licensing

The `sqliters` source code in this repository is dedicated to the **Public
Domain** (see [LICENSE.md](LICENSE.md) / [LICENSE.tr.md](LICENSE.tr.md)):

> The author disclaims copyright to this source code.  In place of
> a legal notice, here is a blessing:
>
>   *   May you do good and not evil.
>   *   May you find forgiveness for yourself and forgive others.
>   *   May you share freely, never taking more than you give.

For jurisdictions that do not recognize direct public-domain dedications, this
repository is additionally offered under **`Unlicense OR CC0-1.0 OR 0BSD`**
(zero-clause / zero-attribution public-domain-equivalent licenses), strictly
**"AS IS" and without warranty of any kind**.

## Building and Running

`sqliters` is implemented in Rust with **zero external crate dependencies**
(`std` only; `#![forbid(unsafe_code)]` across all upper layers, with minimal
OS VFS/SHM system calls isolated inside `crates/sqliters-vfs/src/sys/`).

Build the workspace and command-line shell with Cargo:

        cargo build --workspace
        cargo build --release -p sqliters-cli

Run the interactive `sqliters` CLI shell or execute single-command queries:

        cargo run -p sqliters-cli -- example.db
        cargo run -p sqliters-cli -- -json example.db "SELECT sqlite_version();"

Run the unit, documentation, cross-process C SQLite compatibility, and adapted
Tcl test suites:

        cargo test --workspace

### Library Example

```rust
use sqliters::{Connection, Value};

fn main() -> sqliters::Result<()> {
    let mut conn = Connection::open_in_memory()?;

    conn.execute(
        "CREATE TABLE items (
             id INTEGER PRIMARY KEY,
             name TEXT NOT NULL,
             qty INT NOT NULL
         );
         INSERT INTO items (name, qty) VALUES ('alpha', 10), ('beta', 25);",
    )?;

    let result = conn.query(
        "SELECT id, name FROM items WHERE qty >= ?1 ORDER BY id;",
        &[Value::Integer(15)],
    )?;

    for row in &result.rows {
        println!("#{}: {}", row.get_i64(0).unwrap(), row.get_str(1).unwrap());
    }
    Ok(())
}
```

## Source Tree Map

  *  **src/** - Root library crate (`sqliters`) re-exporting the public Rust
     API (`Connection`, `Statement`, `Row`, `Value`, `ErrorCode`, `Vfs`).

  *  **crates/sqliters-types/** - Core data types, primary/extended result
     codes, limits, 1..=9 byte big-endian varints, `Value` affinities and
     collations, text encodings (`UTF-8`, `UTF-16le`, `UTF-16be`), and Format 4
     record encoding/decoding.

  *  **crates/sqliters-vfs/** - Pluggable Virtual File System (`Vfs`,
     `VfsFile`), POSIX `fcntl` and Win32 byte-range locking, WAL shared-memory
     (`WalShm`) coordination, and `MemoryVfs`.  Platform FFI is strictly
     confined to `crates/sqliters-vfs/src/sys/`.

  *  **crates/sqliters-pager/** - Page cache, 100-byte SQLite Format 3
     database header, rollback journal modes, hot-journal crash recovery, and
     savepoints.

  *  **crates/sqliters-btree/** - SQLite Format 3 Table B+Tree and Index
     B-Tree storage engine, page splitting/rebalancing, overflow chains,
     freelist management, and `PRAGMA integrity_check`.

  *  **crates/sqliters-syntax/** - SQLite 3.54.1 SQL tokenizer, `is_complete`
     state machine, AST definitions, and recursive-descent / Pratt parser.

  *  **crates/sqliters-vdbe/** - Register-based Virtual Database Engine
     (`VdbeVm`), `EXPLAIN` disassembly, sorters, and built-in scalar,
     aggregate, and UTC date/time functions.

  *  **crates/sqliters-core/** - Schema catalog (`sqlite_schema`), SQL-to-VDBE
     compiler, relational execution engine, foreign-key and conflict-clause
     semantics, and the `Connection` / `Statement` lifecycle.

  *  **crates/sqliters-cli/** - Command-line shell (`sqliters`) supporting
     interactive REPL and dot-commands (`.tables`, `.schema`, `.dbinfo`,
     `.dump`, `.mode`).

  *  **crates/sqliters-tcltest/** and **tests/** - Clean-room Rust Tcl-subset
     test runner and adapted public-domain SQLite `.test` suites alongside
     cross-process compatibility and regression tests.

## How It All Fits Together

`sqliters` is modular in design.  Relevant upstream reference specifications
include the
[SQLite architecture](https://sqlite.org/arch.html), the
[SQLite database file format](https://sqlite.org/fileformat2.html),
[VDBE opcodes](https://sqlite.org/opcode.html),
[atomic commit in SQLite](https://sqlite.org/atomiccommit.html), and the
[query planner overview](https://sqlite.org/optoverview.html).

### Key Upstream C Reference Counterparts

  *  **sqlite.h.in / sqliteInt.h** -> `crates/sqliters-types/` and
     `crates/sqliters-core/src/lib.rs` (public types, error codes, limits,
     connection/statement interfaces).

  *  **tokenize.c / complete.c / parse.y** -> `crates/sqliters-syntax/`
     (lexical analysis, statement completeness, and SQL parsing).

  *  **vdbe.c / vdbesort.c / func.c / date.c** -> `crates/sqliters-vdbe/`
     (virtual machine execution, sorting, and built-in SQL functions).

  *  **build.c / select.c / insert.c / update.c / delete.c / where.c / fkey.c**
     -> `crates/sqliters-core/` (schema resolution, query planning, bytecode
     emission, and constraint enforcement).

  *  **btree.c** -> `crates/sqliters-btree/` (table/index B-tree pages,
     overflow chains, and freelists).

  *  **pager.c / wal.c** -> `crates/sqliters-pager/` and
     `crates/sqliters-vfs/src/shm/` (transactions, rollback journals, and
     WAL-index primitives).

  *  **os_unix.c / os_win.c / memvfs.c** -> `crates/sqliters-vfs/` (operating
     system file I/O, locking, and in-memory VFS).

  *  **shell.c.in** -> `crates/sqliters-cli/` (command-line interface).

  *  **tclsqlite.c / test\*.c / test/\*.test** -> `crates/sqliters-tcltest/`
     and `tests/` (test harness and verification suites).
