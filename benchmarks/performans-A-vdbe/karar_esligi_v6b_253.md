# V6b.253 — Varsayılan sayısal tarih metni

Normal sayısal TEXT, değiştiricisiz veya yalnız çıktı hassasiyeti bayraklı durumda C’nin Julian gün dönüşümünü kullanır. 46 eski C farkı kapandı. STRFTIME terminal yüzde önkoşulu ile 1.944 biçim ve 1.480 native matrisin taze birleşik doğrulaması geçti. Üç dosya profili ve 60 C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v252-strftime-dangling-percent-numeric-composition-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 243/245, determinism 244/245. Açık L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 300,15 ms taban, 380,96 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/raw-numeric-text-datetime-disk-verify/res/gates-validation.txt).
