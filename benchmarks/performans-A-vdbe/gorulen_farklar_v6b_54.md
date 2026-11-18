# V6b.54 — Ölçülen kapsam ve açık farklar

D158, normal sıcak olmayan disk journal yazıcısının `syncJournal` içindeki sonraki başlığı tarama kararına odaklanıyor. Bağımsız nonce ile ham PERSIST hash farkı, özel VFS sync hatalarının tam matrisi, savepoint ve platform VFS yolları açık.

D159 normal WAL COMMIT phase-two bildirimi, EXCLUSIVE rollback COMMIT bildirimi ve 2PC'nin super-journal sync bildirimi ayrı açık farklar. D52 RETURNING sayaç zamanlaması, kalan PRAGMA ve V1/M4 çalışmaları sürüyor. [Karar ve doğrulama](karar_esligi_v6b_54.md).

Y1 VACUUM sonrası bağlantı durumu ve ana dosya boyutu kararları açık.
