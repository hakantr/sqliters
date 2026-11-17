# V6b.44 — Açık farklar ve kapsam

D147 sıcak journal kayıt sırası, kayıt xRead sınırları ve kısa kayıt sonu davranışı ile D148 playback öncesi ana DB uzunluğu, ölçülen akışlarda C ile eşitlendi. Sıcak playback dışındaki savepoint, sub-journal ve MEMORY akışları, kalan pager ERROR durumları ve platforma özgü VFS yolları ayrı kanıt gerektiriyor.

D149 sıcak journal bölüm başlığının okuma sınırı. C `pager.c:1649–1684` magic'i ve 32 bit alanları ayrı xRead ile alıyor. Rust bölüm başlığını tek okuyor. 8 ofsetindeki 4 baytlık nRec okuması hata verirse C ilk sorguda SQLITE_IOERR_READ 266 döndürüyor, sonra eski veriyi kurtarıyor. V6b.44 Rust'ta aynı xRead çağrısı oluşmadığı için ilk sorgu başarılı oluyor. Dört sayfa boyutunda iki Rust kipinde tekrarlandı. Ham kanıt `/home/hakan/github/sqliters/target/perf-work/v6b48/res/baseline.json`.

D52, V1/M4, kalan DDL ve PRAGMA davranışları ile E28'in diğer kurtarma ve hata yolları genel C eşliği açısından açık. [Karar ve doğrulama](karar_esligi_v6b_44.md).
