# V6b.250 — Finder L03 olağan PRAGMA optimize

Normal MAIN kapsamındaki dolu, istatistiksiz tam indeks için varsayılan PRAGMA optimize, C gibi mevcut ANALYZE yoluyla istatistik yazar ve geçici limiti geri yükler. Kanonik senaryonun beş evresindeki C ve Rust dosyaları eş. Bağımsız 588 C salt okunur açılışı ve iki taze tam Finder koşusu geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v248-finder-l03-optimize-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 243/245, determinism 244/245. Açık L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 301,57 ms taban, 379,67 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/finder-l03-optimize-disk-verify/res/gates-validation.txt).
