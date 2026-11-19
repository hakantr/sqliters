# V6b.93 — EXCLUSIVE PERSIST ve TRUNCATE→WAL journal sonlandırması

EXCLUSIVE PERSIST veya TRUNCATE başlangıcında, hedef WAL başlık işleminin rollback journal'ı commit ikinci aşamasında mevcut kaldırma yoluna veriliyor. Aynı karar EXCLUSIVE DELETE için V6b.92'de ölçüldü. Başlık yazımı boyunca rollback journal koruması sürüyor. [C kaynak ve karar](/github/sqliters/target/perf-work/journal-persist-truncate-independent/sonuc.md)

Üç sayfa boyutu, NORMAL/EXCLUSIVE ve iki Rust kipinde 240 sağlıklı geçiş C ile eş. Taban 192/240. 1.440 ana DB imajı bayt düzeyinde eş ve 240 C salt okunur açılışı geçti. D145 ve V6b.92 için 120 DELETE↔WAL koruması ile 720 ek imaj aynı. [Bağımsız kanıt](/github/sqliters/target/perf-work/journal-persist-truncate-independent/res/proof.sha256)

Birleşik optimize adayda OFF 387/400 ve ON 400/400. Tam DML korpusunun ham özetleri V6b.92 ile aynı. [Tam korpus](/github/sqliters/target/perf-work/persist-truncate-wal-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 551,96 ms taban, 465,87 ms aday. Süre oranı 0,844 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/persist-truncate-wal-disk-verify/res/gates-validation.txt).
