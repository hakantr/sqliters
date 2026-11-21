# V6b.347 — Sol INTEGER PRIMARY KEY filtresiyle ters LEFT JOIN taraması

72 SELECT biçimi ve altı profilde 70 sıra farkı kapandı. 18 C yeniden açılışı ve sekiz odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v347-reverse-unordered-filtered-left-join-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1223 geçti, 1 başarısız, 329 yok sayıldı. ON 1220 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,02 ms taban, 383,04 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/reverse-unordered-filtered-left-join-disk-verify/res/gates-validation.txt).
