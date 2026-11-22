# V6b.379 — CTE ve ifade alt sorgusu üzerinden görünüm CASE eşliği

Gerçekten başvurulan özyinelemesiz CTE ve ifade alt sorgularında kalıcı görünüm çözümü korunuyor. C ve iki Rust kipinde 660 gözlemde 130 fark kapandı. Altı C açılışı ve önceki DEFAULT testi geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v379-case-view-references-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1257 geçti, 1 başarısız, 325 yok sayıldı. ON 1254 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 309,90 ms taban, 397,67 ms aday. Süre oranı 1,283 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-view-references-disk-verify/res/gates-validation.txt).
