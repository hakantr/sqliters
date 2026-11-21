# V6b.318 — NOT NULL test paketini etkinleştir

Mevcut NOT NULL paketi iki çalışma kipinde etkin. 106/106 ham Tcl kontrolü, 12 olağan kısıt profili ve 18 C salt okunur dosya kontrolü eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v314-enable-notnull-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1194 geçti, 1 başarısız, 330 yok sayıldı. ON 1191 geçti, 4 başarısız, 330 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 573,67 ms taban, 732,53 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/enable-notnull-workspace-disk-verify/res/gates-validation.txt).
