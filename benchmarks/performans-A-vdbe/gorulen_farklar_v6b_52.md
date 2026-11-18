# V6b.52 — Ölçülen kapsam ve açık farklar

D156 normal NamedDisk ana rollback-journal kayıt yazımlarını kapsıyor. Ortak nonce ile journal baytları doğrudan karşılaştırılır. Bağımsız rastgele nonce ile PERSIST journal kalıntısında ham bayt farkı sürüyor. Bitvec bellek tükenmesi, subjournal VFS I/O sınırları, TEMP/MEMORY ve tüm savepoint hata akışları açık.

D157, SAFE_APPEND ve SEQUENTIAL üzerinde journal başlığı ve sync kararlarını inceliyor. D158, sonraki journal bölümündeki doğal kısa okumanın iki VFS gösterimini inceliyor. D159, dört sağlıklı WAL COMMIT akışında C'nin yaptığı `SQLITE_FCNTL_COMMIT_PHASETWO` bildiriminin yokluğudur. Aynı dört akışta SQL sonucu ve ölçülen ana DB özetleri eşit. 2PC bCleanup bildirimi ayrı karar olarak açık.

D52 RETURNING sayaç zamanlaması, kalan PRAGMA, V1/M4 ve diğer pager/WAL/platform VFS yolları açık. [Karar ve doğrulama](karar_esligi_v6b_52.md).
