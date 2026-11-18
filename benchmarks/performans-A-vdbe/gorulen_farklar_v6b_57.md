# V6b.57 — Ölçülen kapsam ve açık farklar

D52 başarılı prepared INSERT RETURNING satırlarının üst değişiklik sayacını ifade kapanışında yayımlamasını kapsıyor. UPDATE/DELETE RETURNING, OR FAIL, count_changes, karma sanal tablo ters çağrıları ve bütün SAVEPOINT_ROLLBACK kararları ayrı kanıt gerektiriyor.

2PC phase-one bildirimi ve aktif rollback temizliği, tek dosyalı spill geri yazımı, V1 PRAGMA iç hata ofseti, Y1 VACUUM sonrası bağlantı durumu ve kalan PRAGMA/M4 açık. [Karar ve doğrulama](karar_esligi_v6b_57.md).
