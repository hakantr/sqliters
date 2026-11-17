# V6b.25 — defer_foreign_keys; D130 ve açık D131

Prepare etkisi, eski programın bayrağı ile live flag ayrımı, normal FK/commit/rollback/reset ve şema yükleme temizliği hizalandı. 1644 akışın 1624’ü tam aynı; 20 eski D52 RETURNING sayaç farkı korunur. Bütün .db baytları aynı. Ek 60 normal/bozuk olmayan qualifier/autocommit, 96 karma FK ve 12 RESTRICT akışı tam eşittir.

D130 düzeltildi: erken RESTRICT hatası yerine parent yazımı sonrası FK eylemi, C 1811 kodu ve statement-journal gerekliliği korunur. PRAGMA içermeyen 12 örneğin önceki ikilide farklı olan 10 dosyası artık C ile aynı. D52’de önceki/sonraki legacy davranış değişmedi.

D131 AÇIK: çalışan EXPLAIN INSERT yazıcı sayılmadığı için yanındaki okuyucu defer bayrağını erken temizler; altı örneğin altısı farklı. Genel yazıcı sayacı/kapanış, E28, D103 ve kalan işler sürer. 647 alınmayan dal ayrı sınıflandırıldı. Kanıt [karar_esligi_v6b_25.md](karar_esligi_v6b_25.md) içinde.
