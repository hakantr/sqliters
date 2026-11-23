# V6b.466 — VACUUM rollback fiziksel sync hatası ve yeniden açılış

İlk 778 korunurken sonraki okuma fiziksel rollback sync hatasını 1034 olarak bildiriyor ve hata kalkınca sıcak journal olağan yoldan oynatılıyor. Dört çift yönlü doğrulama ve 108 dosya imajı geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v466-vacuum-rollback-sync-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v466-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1340 geçti, 0 başarısız, 325 yok sayıldı. ON 1340 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,76 ms taban, 384,99 ms aday. Süre oranı 1,280 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vacuum-rollback-physical-sync-disk-verify/res/gates-validation.txt).
