# V6b.58 — Ölçülen kapsam ve açık farklar

D162 normal disk katılımcılarında phase-one bildirimi, commit noktası ve aktif rollback sonrası korumalı super-journal temizliğini kapsıyor. Tam xAccess ve journal xRead izleri, guard'ın tahsis ve geri kalan VFS hata kararları ile uzun yol sınırı ayrı ölçüm gerektiriyor.

Tek dosyalı rollback'te altı NORMAL/FULL spill vakasında eski ek geri yazım sürüyor. D163 için ayrı aday hazır. Dört PERSIST ham dosya farkı bağımsız checksum nonce'larına bağlı. V1 PRAGMA iç hata ofsetleri, Y1 VACUUM bağlantı durumu, kalan PRAGMA/M4 ve özel WAL/VFS akışları açık. [Karar ve doğrulama](karar_esligi_v6b_58.md).
