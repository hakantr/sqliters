# V6b.468 — VACUUM rollback sync bildirimi ve gerçek sync sırası

Kesin 1034 bildirimi fiziksel sync öncesinde C gibi dönüyor. Hata sürerken sonraki okuma yeniden 1034 alıyor, hata kalkınca olağan kurtarmada ana dosya sync olup FK ve önceki veri okunuyor. Fiziksel sync, close ve kilit kararları ayrı doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v468-vacuum-rollback-sync-notification-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v468-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1341 geçti, 0 başarısız, 325 yok sayıldı. ON 1341 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 306,22 ms taban, 392,07 ms aday. Süre oranı 1,280 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vacuum-rollback-sync-notification-disk-verify/res/gates-validation.txt).
