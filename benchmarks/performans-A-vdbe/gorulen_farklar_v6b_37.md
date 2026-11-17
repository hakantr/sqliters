# V6b.37 — Görülen farklar

468 akışın kalan 66 farkı WAL ve auto_vacuum sonrası V1 yenilemesi ile boş dosyada savepoint geri alımına ait. 42 akışın ham sonucu V6b.36 ile aynı. Diğer 24 V1 savepoint akışının satır ve hata çıktısı C’ye ulaştı. Dosya hash'i V6b.36’nın V2 eş akışıyla aynı kaldı. Örnek dosyada 48 farklı bayt, C sayfa 1'de mantıksal olarak silinmiş katalog hücresinin içeriği ve hücre bölgesi işaretçisinde kaldı. `res/savepoint_sample/` bu örneğin iki dosyasını içerir.

Gerçek şema değişikliği matrisi 24/48. V1 `sqlite3_step` eskimeyi genel hata olarak bildirirken Rust 17 ve ayrıntılı iletiyi döndürüyor. Kalan G/Ç ve TEMP kapsamı, D52 sayaçları, D103/D143, E28 ve tam DDL makine yürütümü açıktır. [Karar ve doğrulama](karar_esligi_v6b_37.md).
