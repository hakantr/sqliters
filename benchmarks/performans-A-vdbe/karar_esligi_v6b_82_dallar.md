# V6b.82 — Tek satırlı FK C dal kapsamı

`fkey.c:440–455` immediate dalında tek satırlı ve uygun INSERT için `Halt 787`. Farklı loop, deferred veya trigger koşulunda mevcut `FkCounter` yolu. 54 C EXPLAIN ve üç kodlama, immediate/deferred, trigger, explicit işlem, savepoint, geçerli parent ve açık sütunlu RETURNING kontrollerinde ölçüldü. [C kaynak ve opcode kapsamı](/github/sqliters/target/perf-work/v82-default-fk-independent/sonuc.md).
