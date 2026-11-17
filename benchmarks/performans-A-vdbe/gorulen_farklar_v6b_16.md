# V6b.16 — shrink_memory ve D117

shrink_memory hazırlama etkisi taşındı. 13.181 tam program ve 4.157/4.158 API akışı aynı; tek çıktı farkı önceki D103. Bütün 4.158 akışın .db baytları aynı. Prepare/ilk step/reset, EXPLAIN/EQP ayrımı ve açık imleç karşılaştırıldı.

72 VFS örneğinde DB sayfa okumaları ve .db baytları aynı; ham izlerin yalnız 10’u tamamen aynı. Başlık okuma aralığı, rollback journal okuma ve TEMP dirty cache ömrü gözlemleri açık.

**D117 açık:** sıcak sorguda ilk 24 bayta dokunan VFS okumaları SQLITE_IOERR_READ döndürünce C başarılı; Rust iki anahtarda 10/266 disk I/O error verir. Sonraki iş bu hata davranışını düzeltmektir.

Kanıt ve sınırlar [karar_esligi_v6b_16.md](karar_esligi_v6b_16.md) içinde. Genel C eşliği henüz sağlanmadı.
