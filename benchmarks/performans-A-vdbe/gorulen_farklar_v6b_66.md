# V6b.66 — Açık eşlik maddeleri

Boş WAL başlığı ve örtük kapanış callback'i ölçülen olağan kapanış/okuma senaryolarında eşit. TRUNCATE sonrasında C'nin SHM hash alanında koruduğu iki eski hücre Rust'ta sıfırlanıyor. Bu fark aynı taban şartlarında önce de vardı. Commit, checkpoint sonucu, DB özeti ve doğrulanan WAL checksum'ları ölçülen akışta eşit. [Ham WAL kaydı](/home/hakan/github/sqliters/target/perf-work/empty-wal-publish-proof/res/candidate/comparison.txt).

Sıfırdan uzun kısa WAL boyutları ayrı kapsam. Sanal tablo rollback yaşam döngüsü, checkpoint G/Ç hata kodu ve doğrudan pager kapanışı açık. Açık transaction fazında rollback journal, WAL ve SHM ham SHA farkları rastgele alanlar ve bunlara bağlı checksum'larla sınırlı ölçüldü. Son kapanış ve yeniden açılış dosya kümeleri eşit. [Birleşik ham kayıt](/home/hakan/github/sqliters/target/perf-work/v6b66-close-combined-proof/res/verified.txt) · [D167 kapanış kapsamı](gorulen_farklar_v6b_63.md).

SQL tarafında sayısal BLOB koşul dönüşümü, stored trigger REGEXP hazırlama tanısı ve MATCH V1 step/reset kararı V6b.64–65 kayıtlarında açık. [Önceki durum](gorulen_farklar_v6b_65.md).
