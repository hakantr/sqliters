# V6b.167 — NO_CKPT_ON_CLOSE bağlantı ayarı

SQLite C'nin bağlantıya özgü NO_CKPT_ON_CLOSE ayarı Rust bağlantı API'si ve CLI'de aynı durumla çalışır. Etkin değer, kapanış ve DETACH sırasında otomatik WAL checkpoint hazırlığını atlar. Açık wal_checkpoint ve journal kipi geçişi önceki kararlarını izler. [Bağımsız C ve Rust kaydı](/github/sqliters/target/perf-work/v161-no-ckpt-on-close-independent/sonuc.md)

702 doğal akışta 819 WAL pager kapanış sınırı, 321 etkin ayarlı dosya koruması, 426 otomatik checkpoint ve 72 canlı peer BUSY doğrulandı. 846 C açılışı ve 18 CLI süreci geçti. [7.037 strict kayıt](/github/sqliters/target/perf-work/v161-no-ckpt-on-close-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/no-ckpt-on-close-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,27 ms taban, 378,95 ms aday. Süre oranı 1,250 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/no-ckpt-on-close-disk-verify/res/gates-validation.txt).
