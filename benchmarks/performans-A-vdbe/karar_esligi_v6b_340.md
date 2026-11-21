# V6b.340 — Ayrı indeks ANALYZE istatistiklerinin kullanımı

Tek indeks ANALYZE sonrasında C’nin varsayılan satır tahmini uygulanıyor. Ölçülen 120 profildeki 590 fark kapandı. Yazılan dosya, kalıcı sıra ve 360 C yeniden açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v340-wr-covering-index-analyze-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1216 geçti, 1 başarısız, 329 yok sayıldı. ON 1213 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,36 ms taban, 383,54 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wr-covering-index-analyze-disk-verify/res/gates-validation.txt).
