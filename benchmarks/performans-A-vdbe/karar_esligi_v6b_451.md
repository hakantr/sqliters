# V6b.451 — Yetkilendiricili doğrudan function_list yürütmesi

Geçerli izin, yoksayma ve ret kararı korunarak doğrudan PRAGMA programı C ile 312 yaşam döngüsü profili ve 35.424 olayda eşleşti. İlk adım, reset, V1/V2, tekrar yetkilendirme ve iptal doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v451-function-list-authorized-vdbe-rebased-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v451-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1330 geçti, 1 başarısız, 325 yok sayıldı. ON 1329 geçti, 2 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,42 ms taban, 382,30 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/direct-function-list-authorized-vdbe-disk-verify/res/gates-validation.txt).
