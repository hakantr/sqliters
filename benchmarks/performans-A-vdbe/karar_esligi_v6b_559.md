# V6b.559 — Gerçek rowid anahtarında uygulamanın BINARY sıralaması

Her kipte 774 doğrudan C profili, 300 hata/reset ve gerçek PMA geçti. UTF-8 kaydı, UTF-16 slotu, V1/V2 ve 3.498 dosya sağlığı/SHA gözlemi eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/binary-rowid-override-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v559-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1433 geçti, 0 başarısız, 319 yok sayıldı. ON 1433 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 297,70 ms taban, 388,52 ms aday. Süre oranı 1,305 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/binary-rowid-application-sort-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
