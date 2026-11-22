# V6b.419 — Alt sorgu ve birden çok IN listeli WITHOUT ROWID OR

İki sabitlenmiş alt sorgu biçimi, tam sayı aritmetiği ve bağımsız liste kollarında eski 64 UTF-16 farkı ile 48 komşu fark kapandı. 168 tam hedef planı C ile eşleşti. Önceki IN, filtre, yetkilendirici, kolasyon ve dosya denetimleri korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v419-without-rowid-or-in-producers-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1299 geçti, 1 başarısız, 325 yok sayıldı. ON 1297 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 306,32 ms taban, 391,60 ms aday. Süre oranı 1,278 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-in-producers-and-arms-disk-verify/res/gates-validation.txt).
