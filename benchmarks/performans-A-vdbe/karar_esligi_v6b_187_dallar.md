# V6b.187 — Katılımcı ve fiziksel yazma sırası

İki gerçek katılımcıda EXCLUSIVE bağlı dosyadan ana dosyaya, terminal sayfa 1 yazısı ana dosyadan bağlı dosyaya ilerler. Bağlı dosya zaten WAL ise yalnız ana dosya yazılır. Geçici mantıksal kip, ilk gerçek okuma ve doğal BUSY sonrası taze tekrar gözlemleri ayrı ölçüldü. [Fiziksel kaynak ve C kanıtı](/github/sqliters/target/perf-work/v185-attached-empty-main-wal-staging-independent/sonuc.md)
