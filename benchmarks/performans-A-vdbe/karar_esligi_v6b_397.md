# V6b.397 — UTF-16 satır CASE fiziksel RTRIM indeksleri

480 gözlemde fiziksel RTRIM aramaları ve indeks üzerinden gruplama 104 UTF-16 sonucunu eşitledi. C ile 312 plan eş, önceki 558 gözlem korundu. C fixture 19/19, ilişkili dört Rust testi iki kipte geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v397-row-case-rtrim-physical-preflight/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1277 geçti, 1 başarısız, 325 yok sayıldı. ON 1275 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,00 ms taban, 379,21 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-physical-rtrim-disk-verify/res/gates-validation.txt).
