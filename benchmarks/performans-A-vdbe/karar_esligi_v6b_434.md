# V6b.434 — Doğrudan function_list hazırlama ve reset

Pinli C SQLite üç kodlamada V1/V2 ve Rust OFF/ON ile karşılaştırıldı. 48 akış ve 390 API kaydında ilk adım, reset, aynı aritede değiştirme ve kaldırma, yeniden hazırlama ve finalize eş. 168 kapsam dışı kontrol ve her kipte 27 odak testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v434-function-list-prepare-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1313 geçti, 1 başarısız, 325 yok sayıldı. ON 1311 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,21 ms taban, 391,29 ms aday. Süre oranı 1,286 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-function-list-rows-disk-verify/res/gates-validation.txt).
