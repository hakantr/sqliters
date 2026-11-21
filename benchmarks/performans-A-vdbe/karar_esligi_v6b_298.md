# V6b.298 — Date3 ve func2 normal workspace

İki testin eski ignore satırı kaldırıldı. Dört normal odak koşusu geçti. Ayrı Tcl doğrulamasında her kipte date3 126/126 ve func2 131/131. Tam workspace için iki ek geçen ve iki eksik ignore denetlenir. [Bağımsız kanıt](/github/sqliters/target/perf-work/v292-enable-date3-func2-workspace-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,02 ms taban, 380,14 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/enable-date3-func2-workspace-disk-verify/res/gates-validation.txt).
