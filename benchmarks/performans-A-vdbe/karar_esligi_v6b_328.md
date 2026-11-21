# V6b.328 — Özyinelemeli CTE akışında WHERE çağrı sayısı

Sınırlı olağan özyinelemeli CTE akışında WHERE aynı satır için bir kez çalışır. Beş çağrı sayısı farkı kapandı. 62 sorgu biçimi ve 18 C salt okunur açılış aynı, altı Rust kontrolü iki kipte geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v328-recursive-stream-where-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1204 geçti, 1 başarısız, 329 yok sayıldı. ON 1201 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,93 ms taban, 379,19 ms aday. Süre oranı 1,252 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/recursive-stream-where-disk-verify/res/gates-validation.txt).
