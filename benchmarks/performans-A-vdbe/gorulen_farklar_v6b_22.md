# V6b.22 — collation_list

D126 liste/kayıt geçmişi, D127 UTF-8 API BUSY/expire ve D129 kabuk uint/decimal kayıtları bu dilimde giderildi. 438 native akışta 426 aynı; kalan 12 kesin SELECT hata satırı D128 olarak açıktır. Bütün dosya baytları aynıdır. 39 geçmişte 1362 program, 7536 karşılaştırıcı girdisi, 36 kabuk akışı ve altı bozuk dosya/no-schema akışı eşleşti.

D128’de C hazırlamada 257 verir; Rust açık UTF-8 yolda 1, diğer örneklerde hazırlama başarısı verir. Sıradaki doğruluk işidir. Genel OOM/tahsis izi, başarısız kısmi schema yükleme, builtin silme/sentezleme ve kodlama API yönleri tamamlanmadı. 589 alınmayan dal ayrı sınıflandırıldı; diğer V6b/M4 ve eski farklar açık. Kanıt [karar_esligi_v6b_22.md](karar_esligi_v6b_22.md) içinde.
