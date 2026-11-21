# V6b.280 — strftime on iki saat alanı

Açık TEXT biçimi ve tek sabit tarih girdisinde yüzde l, C'nin boşluk dolgulu on iki saat alanını üretir. Tcl date-3.29 kapandı. Üç kodlamada 2.034 sonuç, 18 C salt okunur açılışı ve önceki 27 W ile 42 date-19 koruması doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v278-strftime-hour-unpadded-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,81 ms taban, 383,26 ms aday. Süre oranı 1,274 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/strftime-hour-field-disk-verify/res/gates-validation.txt).
