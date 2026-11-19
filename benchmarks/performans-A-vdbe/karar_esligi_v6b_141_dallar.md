# V6b.141 — Sonlu ve varsayılan boyut sınırı

Sonlu sınırda boş WAL ile DB sayfa 1 sürümü bitişte (1,1) olur. Varsayılan sınırda kalan WAL yeni sayfa 1 frame'ini taşır ve temiz tekrar açılış C kararını izler. Normal bağlantı kapanışı ve PERSIST kapalı kontrolleri eş. [Fiziksel kayıt](/github/sqliters/target/perf-work/v139-wal-exit-persist-api-independent/res/physical-audit.txt)
