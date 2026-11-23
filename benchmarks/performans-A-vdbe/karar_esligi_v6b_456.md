# V6b.456 — Y1 COMMIT hatasından sonra işlem ve dosya durumu

Yazma, kalıcı yazma, sync ve journal silme hatası ile geç truncate pinli C ve iki Rust kipinde işlem durumu, veri ve FK bütünlüğüyle karşılaştırıldı. Var olan Y1 testi C otomatik geri alma sözleşmesini, geç truncate evresinde ise kaydedilmiş geçerli veriyi doğruluyor. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v455-y1-commit-rollback-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v456-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1334 geçti, 0 başarısız, 325 yok sayıldı. ON 1334 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 305,62 ms taban, 388,39 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/y1-commit-error-rollback-disk-verify/res/gates-validation.txt).
