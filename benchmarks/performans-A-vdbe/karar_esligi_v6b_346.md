# V6b.346 — INTEGER PRIMARY KEY için ters USING JOIN taraması

50 SELECT biçimi ve altı profilde 70 sıra farkı kapandı. 18 C yeniden açılışı ve yedi odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v346-reverse-unordered-key-using-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1222 geçti, 1 başarısız, 329 yok sayıldı. ON 1219 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 307,96 ms taban, 387,73 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/reverse-unordered-key-using-disk-verify/res/gates-validation.txt).
