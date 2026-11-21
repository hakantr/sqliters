# V6b.302 — Tarih girdisinde now yazımı

Ortak now girdi kararı çevre boşluğunu kabul etmez. 2.880 satır sonucu ve 900 C salt okunur dosya doğrulandı. Doğal adım saati, date3 ve func2 korumaları geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v294-date-now-exact-lexical-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 308,49 ms taban, 393,63 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-now-exact-lexical-disk-verify/res/gates-validation.txt).
