# V6b.53 — Ölçülen kapsam ve açık farklar

D157 cihaz yeteneği ve sağlıklı spill kararlarına odaklanıyor. MEMORY journal ve açık journal bulunmayan durumlar, özel cihaz VFS'inde sync ve yeni başlık yazma hataları, aktif savepoint başlık ofsetleri ve platform VFS'e özgü diğer akışlar ayrı kanıt gerektiriyor.

D158 için bir sonraki hizalı journal başlığındaki normal EOF ile `SQLITE_IOERR_SHORT_READ` dönüşü arasındaki C/Rust farkı yeniden üretildi. D159 WAL COMMIT bildirimi ve 2PC bCleanup bildirimi ayrı iş. D52 RETURNING sayaç zamanlaması, PRAGMA, V1/M4 ve diğer pager/WAL yolları açık. [Karar ve doğrulama](karar_esligi_v6b_53.md).
