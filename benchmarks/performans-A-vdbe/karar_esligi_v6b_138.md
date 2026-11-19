# V6b.138 — Mevcut izinli OFF kipinden WAL'a geçiş

Normal NamedDisk zaten izinli OFF kipindeyken WAL'a geçiş bekleyen statement'a aittir. İlk ROW sırasında sayfa 1 (1,1) kalır. DONE, reset veya finalize ile (2,2) yazımı C boyutu, ofseti ve slot sırasıyla tamamlanır. Bekleyen kip önceki kipi de taşır, MEMORY ve OFF başlangıçları temizleme sırasında ayrılır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v137-off-to-wal-independent/sonuc.md)

864 yeni, 648 önceki kip ve 270 Defensive akışı C ile eş. 504 önceki fiziksel sınır farkı kapandı. [Strict native kayıt](/github/sqliters/target/perf-work/v137-off-to-wal-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/off-to-wal-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,45 ms taban, 382,79 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/off-to-wal-disk-verify/res/gates-validation.txt).
