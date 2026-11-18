# V6b.47 — Açık farklar ve kapsam

D151 sıcak PERSIST/TRUNCATE sonlandırması ölçülen sorgu ve dosya yollarında eşit. Normal commit akışında journal'ın NO_LOCK öncesinde kapanması artık eşit. C'nin normal commit sonundaki ayrı SHARED xUnlock çağrısı Rust'ta hâlâ eksik. Ham normal ve EXCLUSIVE tanıkları `/home/hakan/github/sqliters/target/perf-work/v6b51_pager/witness-candidate.json` içinde.

D152 sıcak playback ana dosya sync file-control bildirimi. C `pager.c:6462–6472` NOTFOUND'u başarıya çeviriyor, OFF kipinde de bildirimi gönderiyor ve bildirim G/Ç hatasını sync ve sonlandırmadan önce döndürüyor. Rust VFS'de bu çağrı açık. C ölçümü `/home/hakan/github/sqliters/target/perf-work/v6b52-proof/res/hot-sync.json` içinde.

D153 bağlantı kapanışında journal sync ve kilit sırası. Ölçülen normal vakalarda C `NO_LOCK`, ardından bağlantı kapanışı yapıyor. `UNDELETABLE_WHEN_OPEN` ile tanıtıcı tutulduğunda C önce açık journal'ı sync ediyor, NO_LOCK yapıyor ve journal'ı kapatıyor. Rust bu vakada doğrudan journal'ı kapatıyor. EXCLUSIVE tanıklarında C'de ayrıca SHARED'a düşme gözlendi. C kaynak noktaları `pager.c:1915–1943,2240–2267,4232–4286`. Ham kayıt `/home/hakan/github/sqliters/target/perf-work/v6b51-proof/device/res/device-retention.json` içinde.

D52, V1/M4, kalan DDL, PRAGMA, savepoint ve platform VFS davranışları ile diğer E28 yolları genel C eşliği için açık. [Karar ve doğrulama](karar_esligi_v6b_47.md).
