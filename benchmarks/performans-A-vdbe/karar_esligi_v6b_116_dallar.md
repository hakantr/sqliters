# V6b.116 — Hazırlık, ilk ROW ve ortak fiziksel bitiş

İki ve üç disk, ters slot hazırlığı ve WAL checkpoint, ileri commit ve ortak journal ayrı ölçüldü. İlk ROW ve normal DONE, reset, finalize veya Drop ile sahiplik ve commit hook kaydedildi. [Exact dal kanıtı](/github/sqliters/target/perf-work/v116-multi-journal-lifecycle-independent/sonuc.md)
