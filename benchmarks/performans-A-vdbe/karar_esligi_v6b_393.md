# V6b.393 — UTF-16 satır CASE kısmi sıralaması

Kısmi ORDER BY ve LIMIT/OFFSET iki matriste 288 UTF-16 sonucunu C ile eşitledi. Önceki 558 gözlem, C planları ve koruma yolları aynı. C 13/13, Rust rowvalue8 25/25 ve delete2 1/1 iki kipte. [Bağımsız kanıt](/github/sqliters/target/perf-work/v393-row-case-builtin-partial-order-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1274 geçti, 1 başarısız, 325 yok sayıldı. ON 1271 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 308,18 ms taban, 387,08 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-builtin-partial-order-disk-verify/res/gates-validation.txt).
