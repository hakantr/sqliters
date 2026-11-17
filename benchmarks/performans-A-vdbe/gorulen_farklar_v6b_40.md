# V6b.40 — Görülen farklar

POSIX kabuğunun normal modül listesi, series, gecikmeli JSON kaydı, cache_size, fsdir ve completion akışları eşleşti. `-safe` altında dosya tablosu kaydolmadı. Modül değiştirme ve hazırlanmış ifadelerin eski bağlı nesneyi tutma ömrü V6b.39'da kaydedilen açık konu.

Geçersiz UTF-8 dosya adları, özel dosya veri okuması, yol yeniden adlandırılırken G/Ç hatası, tahsis hatası ve Windows profili kapsama dahil edilmedi. Genel `.nonce` ve diğer kabuk komutları, D52, E28, V1 hata maskelemesi, kalan DDL ve PRAGMA tamamlanmadı. [Karar ve doğrulama](karar_esligi_v6b_40.md).

E28 güncel adayın çekirdeğiyle tek seferlik `SQLITE_IOERR_WRITE` (778) enjekte edilerek yeniden üretildi. C rollback hook çağırıp autocommit 1 ve eski satırı (1) döndürdü. Rust iki kipte de autocommit 0 ve güncellenmiş satırı (2) döndürdü. Hata kodu ve ileti aynı. Kapanış sonrası üç dosyanın hash'i aynı. Ham çıktı `/home/hakan/github/sqliters/target/perf-work/v6b43/res/` altında.

Genişletilen enjeksiyon aynı sonucu DELETE journal veritabanı yazımında, journal ve veritabanı sync çağrılarında, WAL yazımında ve WAL sync çağrısında verdi. Beş akışın tamamında kapanış sonrası dosya hash'leri eşit kaldı. Canlı işlem ve rollback hook farkı sürdü. `v6b43/res/matrix.json` beş akışın ham kanıtını içerir.
