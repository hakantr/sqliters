# V6b.317 — Tek tırnaklı nitelikli sütun adları

C’nin iki ve üç parçalı sütun adı söz dizimi mevcut nitelikli ayrıştırma yoluna bağlandı. 168 ham olay, 18 C salt okunur açılış ve dosya özetleri eş. Bir ayrıştırma regresyon testi eklendi. [Bağımsız kanıt](/github/sqliters/target/perf-work/v313-single-quoted-column-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1193 geçti, 1 başarısız, 331 yok sayıldı. ON 1190 geçti, 4 başarısız, 331 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,10 ms taban, 378,93 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/single-quoted-column-disk-verify/res/gates-validation.txt).
