# V6b.461 — VACUUM DELETE journal silme hatasından sonra kurtarma

İlk 2570 hatası ve işlem durumu korunurken sonraki erişim C gibi olağan sıcak journal kurtarmasına giriyor. Yinelenen silme hatası ve daha sonraki başarılı okuma eşleşti. Yeniden açılan 30 dosyada FK ve bütünlük kontrolü geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v461-vacuum-journal-delete-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v461-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1337 geçti, 0 başarısız, 325 yok sayıldı. ON 1337 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,19 ms taban, 381,49 ms aday. Süre oranı 1,279 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vacuum-delete-journal-recovery-disk-verify/res/gates-validation.txt).
