# V6b.65 — Açık eşlik maddeleri

REGEXP callback pozitif projeksiyonları 192/192 eşit. OFF kipinde UTF-8 sayısal BLOB koşulunun sekiz NOT ve sekiz WHERE vakası C'den ayrışıyor. Adayın bu 16 ham çıktısı V6b.64 tabanıyla aynı. UTF-16 veritabanlarında C ve Rust sonuçları eşit. [Karar ve doğrulama](karar_esligi_v6b_65.md).

Stored trigger dış INSERT hazırlama tanısı ve MATCH V1 yaşam döngüsü açık. [V6b.64 açıkları](gorulen_farklar_v6b_64.md).

Bağımsız olağan DELETE/WAL kapanış matrisi dosya temizliğini, ana dosyaları ve 90 yeniden açılışın satırlarıyla `integrity_check` sonucunu eşit ölçtü. Örtük kapanış rollback callback'i ve kapalı WAL DB ilk salt okunur açılışındaki SHM başlık baytları yeni açıklar. [Kapanış kayıtları](/home/hakan/github/sqliters/target/perf-work/connection-close-proof/res/comparison.txt) · [SHM farkı](/home/hakan/github/sqliters/target/perf-work/empty-wal-readonly-compat/res/readonly-comparison.json).
