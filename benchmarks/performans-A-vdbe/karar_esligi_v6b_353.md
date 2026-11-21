# V6b.353 — WITHOUT ROWID görünüm UPDATE ve aynı anahtara uygun iki tam indeks

180 profilde 120 sıra veya dosya farkı kapandı. 540 C yeniden açılışı ve dört odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v353-view-update-wr-competing-unique-range-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1229 geçti, 1 başarısız, 329 yok sayıldı. ON 1226 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,13 ms taban, 384,89 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-competing-unique-range-disk-verify/res/gates-validation.txt).
