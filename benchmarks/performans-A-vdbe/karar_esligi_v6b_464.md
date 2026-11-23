# V6b.464 — VACUUM kalıcı yazma hatası ve sıcak journal

C ile ilk 778, hata sürerken sonraki okuma ve hata kalkınca aynı bağlantıda eski verinin güvenli kurtarması eşleşti. Dört çapraz salt okunur ve yazılabilir açılışta FK, bütünlük ve dosya baytları doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v463-vacuum-persistent-write-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v464-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1339 geçti, 0 başarısız, 325 yok sayıldı. ON 1339 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,50 ms taban, 385,83 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vacuum-persistent-write-recovery-disk-verify/res/gates-validation.txt).
