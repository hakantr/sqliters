# V6b.60 — Ölçülen kapsam ve açık farklar

D164 otomatik `pragma_locking_mode` için V1 iç şema tanısına odaklanıyor. Genel legacy TVF yürütümü, benzer tanı üreten diğer modüller, bütün V1 eskime akışları ve V1/M4 ayrı doğrulama gerektiriyor.

Prepared UPDATE/DELETE RETURNING ilk satır sayaçlarında legacy farkı açık. Altı sağlıklı WAL kapanış vakasında SQL ve dosya içerikleri eşit. VFS kalıcılık sorgusu ve terminal kilit çağrılarının eşliği ayrıca inceleniyor. Kalan PRAGMA, savepoint ve Y1 VACUUM bağlantı kararları da açık. [Karar ve doğrulama](karar_esligi_v6b_60.md).
