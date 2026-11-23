# V6b.453 — Taşma sayfasında doğrudan kısmi okuma

C kaynaklı dosyada dört indeks araması ve NOT INDEXED taramasında iki Rust kipi C ile aynı fiziksel boyut ve ofseti kullandı. Son 511 baytlık okuma eşleşti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v452-direct-overflow-read-experiment/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v453-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1330 geçti, 1 başarısız, 325 yok sayıldı. ON 1330 geçti, 1 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,28 ms taban, 382,57 ms aday. Süre oranı 1,283 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/direct-overflow-prefix-disk-verify/res/gates-validation.txt).
