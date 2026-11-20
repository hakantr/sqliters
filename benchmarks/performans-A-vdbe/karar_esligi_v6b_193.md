# V6b.193 — Birden çok ATTACH ile koordineli WAL çıkışı

İki ve üç bağlı dosya için C’nin WAL kapatma hatası, sayfa 1 ve şema hazırlığı için kullanılan yeniden deneme yolundan ayrı. Rust bu ayrımı koordineli slotlara uyguluyor. 630 C–Rust çiftinde çağrı sayısı, gözlenen kip, hata dönüşü, ana DB ve yan dosya evreleri eş. 946 fazla callback kaldırıldı. OFF tekrarındaki 56 erken ilk satır yazısı C ile aynı işlem bitişine taşındı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v190-many-attached-wal-exit-busy-independent/sonuc.md)

Birleşik ikilide 32 taze profil, 96 işlem akışı, 56 hedef C çifti, sekiz tek veya bir bağlı dosya koruması ve 318 C yeniden açılışı doğrulandı. [Birleşik tekrar](/github/sqliters/target/perf-work/v193-combined-stage-replay-zero-pad.TPt7ODg3/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,26 ms taban, 379,51 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/many-attached-wal-exit-busy-retry-disk-verify/res/gates-validation.txt).
