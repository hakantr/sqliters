# V6b.481 — Hazır V1 LIMIT/OFFSET tür uyuşmazlığı ve reset

V1 hata adımı genel kodu, reset/finalize özgün 20 ve tanıyı veriyor. Açık ve otomatik tekrar dahil 1.536 API profili C ile eş. Dört Group65 testi iki kipte geçti ve altı dosya FK sağlığını korudu. [Bağımsız ölçüm](/github/sqliters/target/perf-work/prepared-limit-offset-lifecycle-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v481-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1351 geçti, 0 başarısız, 325 yok sayıldı. ON 1351 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,48 ms taban, 387,09 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-limit-mismatch-diagnostic-disk-verify/res/gates-validation.txt).
