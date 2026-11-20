# V6b.234 — Backup hedefi kısmi yazısı

Olağan eş sayfa boyutlu named-disk DELETE, PERSIST ve TRUNCATE yedeğinde ilk ardışık hedef kopyası sayfalayıcı edinme/yazma/bırakma sırasını kullanır. 28 odak profilinde 56 C–Rust geçici yazı ve erken finish çifti eşleşti. Önceki 14 rollback sıra farkı kapandı. 504 backup korumasında 90 ilk kopya ve 48 fiziksel yayın eşliği korundu. 144 geniş profilde 864 son dosya çifti eş. 108 olağan rollback kontrolünde önceki işlem sırası korundu. 32.940 strict kayıt doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v232-backup-destination-spill-rebase-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,74 ms taban, 373,98 ms aday. Süre oranı 1,248 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/backup-destination-spill-disk-verify/res/gates-validation.txt).
