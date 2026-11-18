# V6b.48 — `sqlite3PagerSync` C dalları

SQLite 3.54.1 kapsam nesnesi 20 native sıcak DELETE vakasında 27 `sqlite3PagerSync` çağrısı kaydetti. Aşağıdaki altı çıkışın tamamı alındı. Ham gcov `/home/hakan/github/sqliters/target/perf-work/v6b52-proof/final-worktree/res/gcov-scoped.txt` içinde.

| C yeri | Gcov çıkışı | Karar | Rust karşılığı |
|---|---:|---|---|
| `pager.c:6466` | branch 0, 19 | NOTFOUND sonucu başarıya dönüşür. | `sqliters-pager/src/lib.rs:6499–6502` |
| `pager.c:6466` | branch 1, 8 | OK veya bildirim I/O hatası aynen kalır. | `sqliters-pager/src/lib.rs:6499–6502` |
| `pager.c:6467` | branch 0, 23 | Bildirim başarılı. noSync seçimini değerlendir. | `sqliters-pager/src/lib.rs:6499–6505` |
| `pager.c:6467` | branch 1, 4 | Bildirim hata verdi. Fiziksel xSync atlanır. | `sqliters-pager/src/lib.rs:6499–6502` |
| `pager.c:6467` | branch 2, 18 | Synchronous etkin. Fiziksel DB xSync çağrılır. | `sqliters-pager/src/lib.rs:6504–6505` |
| `pager.c:6467` | branch 3, 5 | Synchronous OFF. Fiziksel DB xSync atlanır. | `sqliters-pager/src/lib.rs:6504–6505` |
