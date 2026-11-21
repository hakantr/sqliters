# V6b.315 — CAST Tcl paketinin etkinleştirilmesi

Belgelendirilmiş Tcl BLOB bağlama seçenek dizilimi tanındı. CAST iki Rust kipinde 130/132’den 132/132’ye geçti. Var olan CAST paketi normal workspace doğrulamasına alındı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v311-cast-tcl-bind-compat-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1189 geçti, 1 başarısız, 334 yok sayıldı. ON 1186 geçti, 4 başarısız, 334 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,51 ms taban, 382,01 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/enable-cast-workspace-disk-verify/res/gates-validation.txt).
