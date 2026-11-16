# V6b.9 — temp_store farkları

D105 (sınanan kapsamda düzeltildi): temp_store yalnız hazırlamayla değişir; setter step sırasında araya giren ayarı geri almaz. EXPLAIN de hazırlama etkisini uygular. Reset sonrası gereken yeniden hazırlama kendi ayarını tekrar uygular. Getter sonucu programa dondurulur.

D106 (sınanan kapsamda düzeltildi): aynı kip açık işlemde bile NOOP’tur. TEMP kapalıyken BEGIN ayarı engellemez; TEMP açıkken !autoCommit veya aktif TEMP Btree işlemi tam C hata metnini verir. İzin verilen değişiklik TEMP’i kapatır, bütün şemaları temizler ve main/aux cache_size değerlerini korur. Salt şema temizleme aktif main VDBE’yi expire etmez. pragma_temp_store kaydı eklendi.

D107 (sınanan kapsamda düzeltildi): sayısal kip yalnız ilk karakterden gelir; 20/2junk MEMORY, 10/1junk FILE; boşluk kırpılmaz. TEMP kapandı diye şema kullanmayan sabit getter yeniden hazırlanmaz; eski sıfır sonucunu korur. İlk adayda bulunan 72 API farkı bu koşulun düzeltilmesiyle kapandı.

D103 önceki adımdan AÇIK: eski yürütücü tüketilmeyen örtük TVF argümanını yeniden denetlemiyor. Girdi karşılaştırmadan çıkarılmadı, başarı sayılmadı. Bilinen 15 açık-anahtar test hatası, DML/ayrışma farkları, E28 ve ayrıca kanıtlanmamış dallar da açık. Ayrıntılar [karar_esligi_v6b_9.md](karar_esligi_v6b_9.md) içinde. Hız ayarı yapılmadı; ileriPlanlama değerlendirilmedi.
