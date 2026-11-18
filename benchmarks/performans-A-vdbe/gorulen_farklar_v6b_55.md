# V6b.55 — Ölçülen kapsam ve açık farklar

D159 normal NamedDisk WAL COMMIT'in bildirimi, sonuç kodu ve commit edilmiş veriyle ilgilidir. EXCLUSIVE ve heap WAL, boş writer, birleşik bildirim ve temizlik hataları, checkpoint ve bağlantı kapanışı ayrı kararlar. Ham WAL ve SHM özetleri her çalışmanın salt ve koordinasyon alanlarını içeriyor.

D160 EXCLUSIVE rollback COMMIT bildirimi ile 2PC super-journal ön sync bildirimi ayrı işler. Y1 VACUUM sonrası bağlantı durumu, D52 prepared INSERT RETURNING sayaçları, PRAGMA ve V1/M4 açık. [Karar ve doğrulama](karar_esligi_v6b_55.md).
