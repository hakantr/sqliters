# V6b.259 — Doğrudan INTEGER/REAL Julian ve hassasiyet

Değiştiricisiz veya yalnız alt saniye hassasiyeti bayraklı INTEGER ve REAL Julian girdileri ortak C uyumlu dönüşümü ve son aralık kontrolünü kullanır. 111 sorguda bit eş, 45 fark kapandı. Üç kalıcı dosya ve 105 bağımsız C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v259-typed-julian-precision-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,08 ms taban, 377,17 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/typed-julian-precision-disk-verify/res/gates-validation.txt).
