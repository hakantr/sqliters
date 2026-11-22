# V6b.395 — Özyinelemeli satır CASE öncelik kuyruğu

360 gözlemde özyinelemeli öncelik kuyruğu için 56 UTF-16 sonucu C ile eşleşti. 558 seçili gözlem aynı. V6b.388 tabanına karşı 2.052 geniş gözlemde kalan 432 fark kapandı. C 13/13 ve Rust rowvalue8 27/27, delete2 1/1 iki kipte. [Bağımsız kanıt](/github/sqliters/target/perf-work/v395-row-case-recursive-priority-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1276 geçti, 1 başarısız, 325 yok sayıldı. ON 1273 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 308,87 ms taban, 389,79 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-recursive-priority-disk-verify/res/gates-validation.txt).
