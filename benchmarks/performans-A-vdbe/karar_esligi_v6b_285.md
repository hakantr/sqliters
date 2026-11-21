# V6b.285 — strftime Pazar başlangıçlı hafta

Açık TEXT biçimi ve tek sabit tarihle yüzde U, C'nin Pazar başlangıçlı haftasını üretir. İki Tcl vakası kapandı ve date-23 adayı 22/22 oldu. Üç kodlamada 2.433 sonuç ve 18 C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v282-strftime-week-sunday-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,94 ms taban, 380,97 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/strftime-week-sunday-disk-verify/res/gates-validation.txt).
