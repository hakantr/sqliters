# V6b.24 — compile_options

PRAGMA compile_options sabit C başvuru derlemesinin 42 seçeneğini aynı sırada üretir; sqlite_compileoption_get/used gerçek makine kitaplığında çalışır. İlk 2.082 akıştaki farklar giderildi. Genişletilmiş 3.990 akışın 10’unda kalan yanlış argüman hata konumu (C 7, Rust -1) da düzeltildi; final 3.990/3.990 aynı ve bütün .db baytları eşit.

COMPILER satırı uyumluluk hedefinin bilgisidir; Rust araç zinciri 1.100’dür. Bu metadata, bütün listelenen özelliklerin tamamlandığı anlamına gelmez. 594 alınmayan dal ayrı sınıflandırıldı. Uzunluk/tahsis hata enjeksiyonu ve genel işlev override yaşam döngüsü kanıtlanmadı. D103, E28, kalan PRAGMA/M4 ve eski farklar açık. Kanıt [karar_esligi_v6b_24.md](karar_esligi_v6b_24.md) içinde.
