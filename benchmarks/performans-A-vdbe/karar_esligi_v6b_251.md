# V6b.251 — ISO saat alanlarının genişliği

Ortak saat ayrıştırması saat, dakika ve saniye alanlarında C gibi iki ASCII rakam ister. 824 sorguluk matriste 84 fark kapandı, 740 eş kontrol ve önceki kesir sonuçları korundu. Üç kalıcı dosya ve 15 bağımsız C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v251-iso-clock-field-width-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 243/245, determinism 244/245. Açık L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 302,38 ms taban, 383,34 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/iso-clock-field-width-disk-verify/res/gates-validation.txt).
