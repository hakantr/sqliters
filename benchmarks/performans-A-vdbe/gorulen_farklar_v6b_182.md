# V6b.182 — Açık kapsam

Sekiz yeni veya sıfır bayt veritabanı ilk ROW anında C hazırlama journalı ile Rust erken sayfa 1 yazımı arasında ayrışıyor. Canlı WAL çıkışı busy handler sayısı, ham sayfa 1 okuma sayısı ve bazı hazırlık sonrası VFS çağrıları ayrı kapsamda. [Açık ölçümlerin raporu](/github/sqliters/target/perf-work/v180-readwrite-setter-prepare-independent/sonuc.md)
