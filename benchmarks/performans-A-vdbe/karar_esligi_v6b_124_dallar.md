# V6b.124 — Checkpoint, ilk ROW ve normal bitiş

Etkin WAL frame'leri ilk ROW öncesinde ana dosyaya taşınıyor. PERSIST ve TRUNCATE journal ömrü, sayfa 1 preimage ve checksum, NORMAL/EXCLUSIVE kilitleme ile V1/V2 adım sınırları ayrı ölçüldü. [Kanıt manifesti](/github/sqliters/target/perf-work/v123-wal-to-retained-step-independent/res/proof.sha256)
