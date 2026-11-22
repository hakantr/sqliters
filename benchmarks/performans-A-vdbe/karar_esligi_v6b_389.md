# V6b.389 — UTF-16 yerleşik satır CASE sıralaması

Tam ORDER BY ve LIMIT/OFFSET NOCASE/RTRIM sonuçları C ile eş. 756, 576 ve 612 gözlemli üç matris 320 farkı kapattı. Eşit anahtarların sıralama kontrolünde 384 Rust sonucu C ile eş ve 256 ek fark kapandı. Önceki 558 sonuç ve yol izi aynı. Pinned C 28/28, iki Rust kipi rowvalue8 21/21 ve delete2 1/1. [Bağımsız kanıt](/github/sqliters/target/perf-work/v389-row-case-builtin-order-sort-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1270 geçti, 1 başarısız, 325 yok sayıldı. ON 1267 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,78 ms taban, 381,51 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-builtin-order-sort-disk-verify/res/gates-validation.txt).
