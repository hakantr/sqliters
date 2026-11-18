# V6b.63 — Açık eşlik maddeleri

D167'nin doğrulaması olağan disk WAL kapanışı ve sentetik kimlik yanıtlarını kapsıyor. Unix dosya kimliği değişiminin platform davranışı kaynak düzeyinde incelendi. Aktif yazıcıyla doğrudan pager kapanışında rollback sırası ve kapanış checkpoint'inin hata kodu açık. C ve Rust ölçümünde SHM olay görünürlüğü farklı. [Karar ve ölçüm](karar_esligi_v6b_63.md) · [Alınmayan dal çıkışları](karar_esligi_v6b_63_dallar.md).

Y1 VACUUM bağlantı durumu, 6038 legacy tarama sırası ve birleşik yıldız genişletme önceki incelemelerde açık kaldı. PRAGMA/M4, savepoint ve platform VFS kapsamı ile çalışma alanındaki bilinen başarısız testler de açık. Bu açıklar ürün düzeyinde tam C eşliği sonucuna engel.
