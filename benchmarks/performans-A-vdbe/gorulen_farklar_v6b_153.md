# V6b.153 — Açık kapsam

Ayrı sağlıklı evrelerde C normal OFF commit sırasında 132 COMMIT_PHASETWO ve SHARED çifti ile WAL→OFF ilk ROW sırasında 36 PERSIST_WAL sorgusu veriyor. Bu aday bağlantı kapanışının terminal NO_LOCK çağrısını kapsıyor. Native Windows ve salt okunur WAL kapanışı açık. [Ham ayrım](/github/sqliters/target/perf-work/v151-off-terminal-no-lock-independent/res/candidate-off/vfs-differences.json)
