# V6b.429 — Unix PMA ve geçici indeks dizini seçimi

64 doğrudan VFS ve 72 gerçek diske taşan SQL profili her Rust kipinde C ile eşleşti. 40 VFS ve 48 SQL farkı kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v429-anonymous-temp-directory-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1307 geçti, 1 başarısız, 325 yok sayıldı. ON 1305 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 296,70 ms taban, 375,73 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/anonymous-unix-temp-directory-disk-verify/res/gates-validation.txt).
