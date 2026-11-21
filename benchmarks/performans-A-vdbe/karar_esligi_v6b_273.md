# V6b.273 — Aynı tamsayı yeniden bağlama

Maskeli yuvada önceki ve yeni typed INTEGER aynıysa C gibi gereksiz yeniden hazırlama oluşmaz. 116 generated ON profilinin sayacı C eş, önceki Rust satırları, kökleri ve dosya baytları korundu. 6.340 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v272-same-int-bind-reprepare-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,55 ms taban, 378,13 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/same-int-bind-reprepare-disk-verify/res/gates-validation.txt).
