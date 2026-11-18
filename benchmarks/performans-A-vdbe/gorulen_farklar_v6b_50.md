# V6b.50 — Açık farklar ve kapsam

D154 normal DELETE, PERSIST ve TRUNCATE için ölçülen commit phase-two kilit davranışıyla eşit. Aktif rollback, özel TEMP/MEMORY/OFF, WAL ve ham backup COMMIT, ayrıca düşük VFS katmanındaki terminal NO_LOCK hata sonucu ayrı ölçüm gerektiriyor.

D155 `SQLITE_FCNTL_COMMIT_PHASETWO`. C `pager.c:2204–2207`, başarılı journal sonlandırması ve DB küçültmesinden sonra, SHARED öncesinde NULL ile bu bildirimi yapar. 24 sağlıklı native vakada C bildirimi yaptı ve varsayılan NOTFOUND'u başarıya çevirdi. V6b.50 Rust terminal izinde bildirim eksik. Ham kayıt `/home/hakan/github/sqliters/target/perf-work/v6b54-proof/res/reference.json`.

D156 disk journal kayıt yazım sınırı. C `pager.c:6078–6093` numara, sayfa ve checksum için üç yazma yapar. Ham PERSIST journal'larında bağımsız rastgele nonce farkı da sürüyor. D157 SAFE_APPEND ve SEQUENTIAL kararları, D52, V1/M4, kalan DDL, PRAGMA, savepoint ve diğer pager/WAL yolları açık. [Karar ve doğrulama](karar_esligi_v6b_50.md).
