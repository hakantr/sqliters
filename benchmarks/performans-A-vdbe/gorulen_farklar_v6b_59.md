# V6b.59 — Ölçülen kapsam ve açık farklar

D163 aktif disk rollback'te senkronize ana journal kaydının geri yazma sınırına odaklanıyor. PERSIST dosyalarında bağımsız nonce ve ona bağlı checksum baytları ham fark olarak ölçülüyor. Subjournal ve tüm savepoint yolları, sıcak playback, VFS'e özgü hatalar ve farklı platformlar ayrıca incelenecek.

V1 iç PRAGMA ofseti, Y1 VACUUM bağlantı durumu, diğer WAL/PRAGMA/M4 kapsamları ve D162'nin kalan guard kararları açık. [Karar ve doğrulama](karar_esligi_v6b_59.md).
