# V6b.445 — Doğrudan function_list EXPLAIN ve ilerleme

Yetkilendirici kurulu olmayan doğrudan kullanımda 24 C ve EXPLAIN izi, 156 adım zamanlaması ve 52.740 API kaydı eş. İptal, reset ve farklı ilerleme periyotları ölçüldü. Hazırlanabilir olmayan Rust-only adlar önceki satır yaşam döngüsünü korur. [Bağımsız kanıt](/github/sqliters/target/perf-work/v445-function-list-direct-vdbe-checked-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1325 geçti, 1 başarısız, 325 yok sayıldı. ON 1323 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,16 ms taban, 384,62 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/direct-function-list-vdbe-disk-verify/res/gates-validation.txt).
