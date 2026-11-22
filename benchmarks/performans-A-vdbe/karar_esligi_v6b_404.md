# V6b.404 — UTF-16 karma rowid ve indeks OR

840 gözlemde karma ana rowid ve ikincil indeks OR için 128 sonuç C ile eş. 192 tam C planı ve önceki 558 gözlem aynı. C fixture 19/19 ve dört Rust odak testi her kipte geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v404-row-case-mixed-rowid-or-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1284 geçti, 1 başarısız, 325 yok sayıldı. ON 1282 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,15 ms taban, 376,26 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-mixed-rowid-or-disk-verify/res/gates-validation.txt).
