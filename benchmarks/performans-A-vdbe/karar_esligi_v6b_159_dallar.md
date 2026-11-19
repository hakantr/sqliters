# V6b.159 — Retained WAL ve takip işlemi

Varsayılan −1 sınırındaki retained WAL ile 36 takip yazısında C frame ve sayfa 1 biçimi eş. Boyut 0 veya 1024 iken WAL aynı inode'da sıfırlandı. PERSIST_WAL 0 olunca normal kapanış yan dosyaları kaldırdı. [Fiziksel denetim](/github/sqliters/target/perf-work/v155-wal-off-persist-independent/res/physical-audit.txt)
