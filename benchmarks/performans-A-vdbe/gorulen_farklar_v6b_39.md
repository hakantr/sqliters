# V6b.39 — Görülen farklar

Native açılış, hazırlanmış liste, tembel kayıt ve kayıt sırası bu matrislerde eşleşti. C, bağlanmış `mod_05` modülü `MOD_05` olarak değiştirildiğinde yeni uygulamadan 505 okuyor. Rust iki kipte de eski uygulamadan 5 okuyor. V6b.38'de de aynı çıktı vardı. Kaydı silen ve başka adla yeniden ekleyen kontroller ile liste sırası eşleşti. Ayrı ömür deneyinde C'nin açık imleci ve değiştirmeden önce hazırlanmış V1/V2 ifadeleri eski modülde kaldı, yeni sorgu yeni modülü gördü. Rust yeni sorguya eski modülü verdi ve V1 ifadeyi erken eskiterek 17 döndürdü. C eski nesneyi son ifade kapanırken, Rust bağlantı kapanırken ayırdı. Ham deney `target/perf-work/v6b41/res/` altında.

C kabuğundaki `fsdir` ve `completion` eksik. Bu durum kabuk modül listesinde de gözüküyor. D52 sayaçları, E28 pager hata durumu, V1 hata maskelemesi ve kalan DDL/PRAGMA işleri sürüyor. [Karar ve doğrulama](karar_esligi_v6b_39.md).
