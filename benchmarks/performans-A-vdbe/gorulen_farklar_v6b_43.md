# V6b.43 — Açık farklar ve kapsam

D146'nın boş DB yanında önceki verinin geri gelmesi, ilk xOpen CANTOPEN/IOERR, kısa ilk bayt okuması ve RW açılışın RO dönmesi vakaları eşitlendi. UNDELETABLE_WHEN_OPEN, SHARED ile EXCLUSIVE arasındaki kaybolma yarışı, tüm xAccess ve kilit hata kombinasyonları ile sıcak journal'ın platforma özgü yolları ayrı kanıt gerektiriyor.

D147 sıcak journal kayıt sırası ve okuma sınırı. Aynı sayfaya ait iki geçerli kayıt yüklendiğinde C ikinci kaydı, V6b.43 Rust ilkini tutuyor. Dört sayfa boyutunda aynı fark tekrarlandı. Kayıt verisi okunurken SHORT_READ enjeksiyonu da C'de playback sonu, Rust'ta ilk sorgu hatası oldu. Baseline `/home/hakan/github/sqliters/target/perf-work/v6b47/res/` altında.

D52, V1/M4, kalan DDL ve PRAGMA davranışları ile E28'in diğer yolları genel eşlik açısından açık. [Karar ve doğrulama](karar_esligi_v6b_43.md).
