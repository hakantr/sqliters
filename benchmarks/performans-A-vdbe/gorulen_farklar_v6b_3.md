# V6b.3 — default_cache_size farkları

D71'in default_cache_size hazırlama kısmı kapandı: normal/EXPLAIN hazırlama önbelleği değiştirir; adımlama yalnız hazırlanmış çerez operandını yazar. Sonraki başka ayarın değeri adımlamada geri alınmaz. RunOnlyOnce yeniden çalıştırmada tekrar hazırlar. Salt okunur üretici incelemesi etki bırakmaz.

D86 (düzeltildi): sıfır default_cache_size çerezi C'deki gibi -2000 döndürür (eski yürütücü 2000); TEMP ayarı gerçek TEMP çerezine yazar (eski yol etkisizdi). Hazırlanmış operand C Atoi/AbsInt32 sonucudur; i32 taşması 0, MIN mutlak değeri MAX olur.

Kanıt [karar_esligi_v6b_3.md](karar_esligi_v6b_3.md) ve v6b3/res/ altındadır. Yeni kaynakta üç yeni regresyon testi var. Çalışma alanı iki kipte denetlendi; bilinen 15 açık test hatası ve DML hata kümeleri değişmedi. Eski beklentiler değiştirilmedi. C API lifetime dizisi iki kipte çıktı/dosya baytlarında aynı.

V6b açık: synchronous, page_size, auto_vacuum ardından kalan işleyiciler/geçmiş; M4 henüz yapılmadı. Hız ayarı yok. ileriPlanlama/ değerlendirilmedi.
