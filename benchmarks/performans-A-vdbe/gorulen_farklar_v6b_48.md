# V6b.48 — Açık farklar ve kapsam

D152 sıcak DELETE playback sync bildirimi, NOTFOUND ve G/Ç hatası yolları ölçülen C ile eşit. C `sqlite3PagerSync` bildirimini aktif rollback, commit ve backup akışlarında da kullanıyor. Bu çağrı noktalarının Rust VFS bildirimi ve `zSuper` argümanı ayrı doğrulama gerektiriyor.

D153 olağan bağlantı kapatma sırası. C `pager.c:4232–4286` açık rollback journal'ı sync eder, boyutunu okur ve ilgili kilit bırakma ve kapatma adımlarını yürütür. V6b.48'de sıcak PERSIST ve TRUNCATE sonrası açık tanıtıcıyla terminal journal sync ve ek NO_LOCK açık. [Önceki ham kapanış tanıkları](gorulen_farklar_v6b_47.md).

D154 normal rollback-journal COMMIT sonrası ayrı SHARED ve son NO_LOCK sırası. D155 `SQLITE_FCNTL_COMMIT_PHASETWO`. D52, V1/M4, kalan DDL, PRAGMA, savepoint, diğer E28 ve platform VFS yolları için genel C eşliği çalışması sürüyor. [Karar ve doğrulama](karar_esligi_v6b_48.md).
