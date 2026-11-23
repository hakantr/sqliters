# V6b.441 — Scalar ve aggregate tanımı değiştirme

Etkin ifadede aynı fonksiyon tanımı üzerine kayıt SQLITE_BUSY verir. Boştayken uygulama callback türü tek liste girdisi bırakacak şekilde değişir. 36 akış, 540 kayıt ve 252 kapsam dışı kontrol doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v440-function-kind-replacement-experiment/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1320 geçti, 1 başarısız, 325 yok sayıldı. ON 1318 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,75 ms taban, 381,42 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/function-kind-replacement-disk-verify/res/gates-validation.txt).
