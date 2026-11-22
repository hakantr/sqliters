# V6b.381 — TEMP görünümünde satır CASE

TEMP görünümü, ana ve bağlı şemalarla C ad çözümlemesini koruyor. 510 gözlemde 80 fark kapandı. 12 C açılışı, görünüm devre dışı bırakma ve yetkilendirme korumaları, bekleyen sorgunun ana görünüme yeniden hazırlanması geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v381-temp-row-case-view-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1259 geçti, 1 başarısız, 325 yok sayıldı. ON 1256 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,21 ms taban, 380,83 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-row-case-view-disk-verify/res/gates-validation.txt).
