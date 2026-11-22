# V6b.369 — Satır değerli CASE karşılaştırması

İki ve üç öğeli CASE karşılaştırmasında 100 fark kapandı. Adım, NULL, karşılaştırma sırası ve geçersiz genişlik tanıları C ile eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v369-literal-row-case-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1246 geçti, 1 başarısız, 328 yok sayıldı. ON 1243 geçti, 4 başarısız, 328 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 307,32 ms taban, 389,54 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/literal-row-case-disk-verify/res/gates-validation.txt).
