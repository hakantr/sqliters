# V6b.94 — EXCLUSIVE MEMORY ve OFF→WAL geçiş journal'ı

EXCLUSIVE MEMORY ve OFF başlangıcında geçiş rollback journal'ı, başlık işleminin ikinci aşamasında kaldırılıyor. Rust sayfa 1 başlık yazımını mevcut geçici rollback journal ile koruyor. C ölçümünde başlık geçişi sırasında disk rollback journal'ı oluşmadı. Başarılı commit ve normal kapanış sınırında journal ömrü eş. [C kaynak ve aday kanıtı](/github/sqliters/target/perf-work/journal-memory-off-independent/sonuc.md)

Üç sayfa boyutu ile 240/240 sağlıklı geçiş, 1.440 ana DB imajının ham SHA-256 eşliği ve 720 C/Rust salt okunur açılışı doğrulandı. Normal son yazıcı kapanışının otomatik checkpoint imajı SHA-256 eş. Önceki DELETE, PERSIST ve TRUNCATE için 240 ek koruma geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/journal-memory-off-independent/res/proof.sha256)

Birleşik optimize adayda OFF 387/400 ve ON 400/400. Tam DML korpusunun ham özetleri V6b.93 ile aynı. [Tam korpus](/github/sqliters/target/perf-work/memory-off-wal-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 297,17 ms taban, 372,59 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/memory-off-wal-disk-verify/res/gates-validation.txt).
