# V6b.432 — Çalışan deyimde yeni aritenin görünürlüğü

Çalışan deyim mevcut fonksiyonu kullanır. Yeni bir hazırlama eklenen ariteyi görür. Her Rust kipinde 36 C vakası eşleşti ve önceki BUSY ile IN kanıtları korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v432-active-scalar-overload-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1309 geçti, 1 başarısız, 325 yok sayıldı. ON 1307 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,13 ms taban, 378,41 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/active-scalar-overload-visibility-disk-verify/res/gates-validation.txt).
