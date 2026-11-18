# V6b.61 — Ölçülen kapsam ve açık farklar

D165 başarılı prepared UPDATE ve DELETE RETURNING üst sayaçlarını kapsıyor. INSERT OR FAIL için ölçülen 33 akış korunuyor. Kısıt sonrası bütün statement savepoint yolları, count_changes, karma sanal tablo çağrıları ve diğer bağlantı ömrü kararları ayrı doğrulama gerektiriyor.

Normal WAL kapanışında VFS kalıcılık sorgusu ve terminal kilit sırası sıradaki doğruluk konusu. Y1 VACUUM bağlantı durumu, kalan PRAGMA/M4 ve özel VFS kararları açık. [Karar ve doğrulama](karar_esligi_v6b_61.md).
