# V6b.356 — WITHOUT ROWID görünüm UPDATE RETURNING için metin biçimli sayısal sınır

450 profilde 480 sıra veya ana dosya farkı kapandı. 1350 C yeniden açılışı ve dört odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v356-view-update-wr-quoted-numeric-returning-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1232 geçti, 1 başarısız, 329 yok sayıldı. ON 1229 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 312,85 ms taban, 412,62 ms aday. Süre oranı 1,319 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-quoted-numeric-returning-disk-verify/res/gates-validation.txt).
