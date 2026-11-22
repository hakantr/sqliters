# V6b.430 — Unix adlandırılmış geçici dosya yolu ve denemeleri

3.252 sınır ve 278 önceki VFS gözlemi doğrulandı. Her Rust kipindeki 36 SQL profili C ile eşleşti. Önceki 12 SQL farkı kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v430-named-temp-boundary-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1307 geçti, 1 başarısız, 325 yok sayıldı. ON 1305 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,71 ms taban, 377,49 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/anonymous-unix-temp-name-disk-verify/res/gates-validation.txt).
