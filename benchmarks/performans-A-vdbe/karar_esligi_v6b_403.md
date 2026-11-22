# V6b.403 — Uygulama kayıtlı skaler kolasyonları

Tek satırlı kaynak tablosuz karşılaştırmalarda kayıtlı NOCASE ve RTRIM çağrıları iki kipte ayrı ayrı 696/696 C eş. Group64 5/5, hata ve reset davranışı geçti. Altı sağlıklı DELETE/WAL imajı ve C yeniden açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v403-row-collation-overrides-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1283 geçti, 1 başarısız, 325 yok sayıldı. ON 1281 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,71 ms taban, 380,80 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/scalar-collation-override-disk-verify/res/gates-validation.txt).
