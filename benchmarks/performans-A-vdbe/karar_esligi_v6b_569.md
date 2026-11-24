# V6b.569 — STRICT WITHOUT ROWID tamsayı anahtarında ayrı TEXT BINARY sıralaması

Her kipte 1248 doğal ve 96 fiziksel PMA profili eş. 600 fallible, 144 fiziksel negatif ve önceki kolasyon kontrolleri geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/binary-strict-without-rowid-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v569-with-v548-correction-proof/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1443 geçti, 0 başarısız, 318 yok sayıldı. ON 1443 geçti, 0 başarısız, 318 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 298,20 ms taban, 388,41 ms aday. Süre oranı 1,303 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/strict-without-rowid-application-binary-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
