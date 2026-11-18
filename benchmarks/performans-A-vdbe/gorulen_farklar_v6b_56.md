# V6b.56 — Ölçülen kapsam ve açık farklar

D160 disk EXCLUSIVE rollback COMMIT bildirimi ve boş EXCLUSIVE/PERSIST durumuna odaklanıyor. Soğuk PERSIST journal kalıntısının bağımsız nonce değerine bağlı checksum alanları ham dosya farkı olarak kaydedildi.

2PC phase-one super-journal sync bildirimi ve hata sonrası aktif rollback temizliği açık. EXCLUSIVE WAL ve diğer özel VFS yolları, Y1 VACUUM sonrası bağlantı durumu, D52 prepared INSERT RETURNING sayaçları, V1 ofsetleri ve kalan PRAGMA/M4 ayrı kapsam. [Karar ve doğrulama](karar_esligi_v6b_56.md).
