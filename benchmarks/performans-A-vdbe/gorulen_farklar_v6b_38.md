# V6b.38 — Görülen farklar

Kayıtlı D103 ve benzer 24 fazla satır akışı kapandı. Bilinmeyen şema tanılarında tırnaklı token iki hazırlama kipinde de korunuyor. Genişletilmiş native matrisin kalan sekiz V1 akışında C, iç PRAGMA'nın 7 bayt ofsetini döndürüyor. Rust -1 döndürüyor. Bu akışlar eşleşme sayılmadı.

Makineye yönlendirme bağlantının otomatik kaydettiği tek `pragma_locking_mode` kaynağıyla sınırlı. Yazar yetkilendirme callback'i, UTF-16, birleşimler ve genel eski TVF yürütümü ayrı doğrulama gerektiriyor. D52, D143/D57, E28, V1 eskime maskelemesi, kalan DDL ve PRAGMA kapsamı açık. [Karar ve doğrulama](karar_esligi_v6b_38.md).
