# V6b.477 — UNIQUE ve NOT NULL toplam anahtarıyla işçi PMA

Tam BINARY UNIQUE ve NOT NULL bileşenleri parse ve etkin okuma şemasında doğrulandı. Her kipte 144 profil, 73.944 C adımı ve 72 reset/reuse eş. Kısmi, ifade, NULL ve özel kolasyon guard’ları ile FK bütünlüğü korundu. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-declared-unique-key-preflight/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v477-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1347 geçti, 0 başarısız, 325 yok sayıldı. ON 1347 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 541,79 ms taban, 657,45 ms aday. Süre oranı 1,213 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/declared-unique-worker-pma-disk-verify/res/gates-validation.txt).
