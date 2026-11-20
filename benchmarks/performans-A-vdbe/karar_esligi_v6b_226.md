# V6b.226 — Backup fiziksel sayfa imajı

Rollback journal kaynağında daha önce kopyalanmış sayfa güncellenirken son başarılı fiziksel yayın kullanılıyor. İlk kopya mevcut kaynak okuyuşunu kullanıyor. 48 hedef imajı, 744 koruma ve 144 geniş işlem profili C ile doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v223-backup-publication-images-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 566,55 ms taban, 720,27 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/backup-published-page-image-disk-verify/res/gates-validation.txt).
