# V6b.355 — WITHOUT ROWID görünüm UPDATE RETURNING için aynı yönlü sınırlar

420 profilde 280 sıra veya dosya farkı kapandı. 1260 C yeniden açılışı ve dört odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v355-view-update-wr-same-direction-returning-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1231 geçti, 1 başarısız, 329 yok sayıldı. ON 1228 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,34 ms taban, 381,37 ms aday. Süre oranı 1,274 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-same-direction-returning-disk-verify/res/gates-validation.txt).
