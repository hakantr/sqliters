# V6b.51 — Açık farklar ve kapsam

D155 normal disk DELETE, PERSIST ve TRUNCATE COMMIT phase-two bildirimi ölçülen tek dosya işlerinde eşit. 2PC'de super-journal silindikten sonraki bCleanup, özel callback dönüşleriyle DB küçültme hatası, EXCLUSIVE, WAL, MEMORY/OFF ve backup akışları ayrı doğrulama gerektiriyor.

D156 disk journal kayıt yazımı. C `pager.c:6078–6093` aynı kayıt için numara, sayfa ve checksum'u ayrı yazar. Rust V6b.51 bunları tek çağrıda yazar. 4096 sayfalı 24 normal C işleminde 4/4096/4 ayrımı ölçüldü. D157 cihaz bayrakları, D158 doğal kısa okuma ve diğer kurtarma yolları sırada.

D52 mevcut prepared INSERT RETURNING testi, OFF üç encoding ve ON UTF-16le/be için sayaç yayım farkını yeniden gösterdi. C `changes/total_changes` değerlerini Halt'a bırakıyor. Rust legacy ilk satırda yayımlıyor. Altı vakada nihai ana dosya SHA-256 eşit. Ham kayıt `/home/hakan/github/sqliters/target/perf-work/d52-proof/res/current.json`. Kalan DDL, PRAGMA, V1/M4, savepoint ve platform VFS yolları da açık. [Karar ve doğrulama](karar_esligi_v6b_51.md).
