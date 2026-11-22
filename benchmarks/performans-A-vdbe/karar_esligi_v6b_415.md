# V6b.415 — Koşullu WITHOUT ROWID OR kolları

Yerel sayısal ve sıralı bileşik filtreler anahtar kopyasından önce çalışıyor. Dört biçimde 64 UTF-16 sonucu ve 96 tam EXPLAIN planı eşleşti. Önceki 14 biçim, uygulama karşılaştırıcısı ve C yeniden açışı korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v415-without-rowid-or-filter-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1295 geçti, 1 başarısız, 325 yok sayıldı. ON 1293 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,46 ms taban, 377,04 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-without-rowid-or-filter-disk-verify/res/gates-validation.txt).
