# V6b.424 — Kullanıcı API üzerinden FILE sıralama geri çağrıları

108 C profili, 15.156 adım, 98.652 tam argüman çifti eş. 84 eski fark kapandı. 60 callback ve 12 VFS açma hatası özgün kodla döndü. [Bağımsız kanıt](/github/sqliters/target/perf-work/v424-public-pma-collation-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1304 geçti, 1 başarısız, 325 yok sayıldı. ON 1302 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 555,59 ms taban, 709,96 ms aday. Süre oranı 1,278 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/public-file-pma-sort-step-disk-verify/res/gates-validation.txt).
