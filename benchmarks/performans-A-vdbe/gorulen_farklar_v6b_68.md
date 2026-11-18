# V6b.68 — Açık WAL eşlik maddeleri

Olağan disk TRUNCATE ve doğal writer restart için eski SHM indeks hücreleri 12/12 iki kipte eşit. Shared restart yordamına erişen MemVfs ve heap-only akışlar farklı doğrulama kapsamı. Ayrı RESTART checkpoint, başarısız G/Ç sonuçları ve tamamlanmamış yazıcının sonraki slot temizliği açık. [Karar ve doğrulama](karar_esligi_v6b_68.md).

Doğrudan pager kapanışı ve kısa WAL kapsamı V6b.66 kaydında. Sayısal BLOB CHECK ve SQL tanı açıkları V6b.67 ve önceki kayıtlarda. [Kapanış açıkları](gorulen_farklar_v6b_66.md) · [SQL açıkları](gorulen_farklar_v6b_67.md).
