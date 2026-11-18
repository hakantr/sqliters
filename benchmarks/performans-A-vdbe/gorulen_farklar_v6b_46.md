# V6b.46 — Açık farklar ve kapsam

D150 normal sıcak DELETE sonlandırması, ölçülen C 3.54.1 Unix VFS kararlarıyla eşit. BATCH_ATOMIC_WRITE derleme yolu, EXCLUSIVE, geçici ve WAL işlemlerinin kendi sonlandırma kuralları bu ölçümün kapsamı dışında.

D151 sıcak PERSIST/TRUNCATE sonlandırması. Kip önceden ayarlanıp bağlantı boştayken sıcak journal oluştuğunda C aynı açık dosya tanıtıcısında PERSIST için ilk 28 baytı sıfırlıyor veya journal'ı boyut kuralına göre kesiyor. TRUNCATE dosyayı boşaltıp yerinde bırakıyor. `pager.c:1436–1468,1915–1940,2124–2142`. Rust `finalize_recovered_journal_file` içinde super-journal olmayan vakalarda dosyayı siliyor. Matrisin 22 sıcak vakasında ayrışma var. PRAGMA'nın ilk komut olduğu iki kontrolde sorgu izleri ve dosya özetleri eşit. Bağlantı kapanışındaki ek C xUnlock çağrısı ayrıca inceleniyor. Ham kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b51_pager/` ve `/home/hakan/github/sqliters/target/perf-work/v6b51-proof/res/` altında.

D152 sıcak playback ana dosya sync file-control'u. C `pager.c:6462–6472` önce `SQLITE_FCNTL_SYNC` bildirimi yapar. NOTFOUND'u başarı kabul eder. Bildirim hata verirse xSync ve sonlandırma atlanır. OFF kipinde bildirim gelir ve fiziksel xSync atlanır. C 20 native vakada bu sırayı verdi. Rust `VfsFile` fiziksel sync sağlar. FCNTL_SYNC bildirimi için karşılık açık. Ham kayıt `/home/hakan/github/sqliters/target/perf-work/v6b52-proof/res/hot-sync.json`.

D52, V1/M4, kalan DDL, PRAGMA ve diğer pager, WAL, savepoint ve platform VFS akışları genel C eşliği için açık. [Karar ve doğrulama](karar_esligi_v6b_46.md).
