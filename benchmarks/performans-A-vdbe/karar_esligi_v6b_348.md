# V6b.348 — WITHOUT ROWID görünüm UPDATE ve bağımsız tam indeksler

96 profilde 100 sıra veya ana dosya farkı kapandı. İki uygun indeksin bulunduğu 24 profil öncekiyle eş. 360 C yeniden açılışı ile altı odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v348-view-update-wr-unrelated-index-revised-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1224 geçti, 1 başarısız, 329 yok sayıldı. ON 1221 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,92 ms taban, 383,61 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-unrelated-index-disk-verify/res/gates-validation.txt).
