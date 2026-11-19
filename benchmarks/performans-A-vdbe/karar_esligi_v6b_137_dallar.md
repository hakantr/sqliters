# V6b.137 — Korumalı ve çoklu disk geçişi

Defensive bağlantıda OFF isteği mevcut kipi bildirdi ve dosyaların baytları ile inode'ları korundu. İki ve üç gerçek katılımcıda ortak journal oluşmadı. Başlangıçta retained kipteki diğer disklerin journal yaşamı C sırasını izledi. [Native fiziksel denetim](/github/sqliters/target/perf-work/v136-wal-to-off-independent/res/physical-audit.txt)
