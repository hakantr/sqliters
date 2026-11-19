# V6b.95 — EXCLUSIVE WAL→MEMORY/OFF journal sonlandırması

EXCLUSIVE WAL→MEMORY/OFF geçişinde, rollback kiplerinin kendi journal ömrüyle sayfa 1 için kullanılan geçici DELETE journal'ın ömrü ayrılıyor. Geçiş commit'inin ikinci aşaması geçici journal'ı C'nin başarı sınırında kaldırıyor. Rust'ın WAL sayfa 1 geçişi için mevcut geçici koruması sürüyor. [C kaynak ve bağımsız karar](/github/sqliters/target/perf-work/wal-to-rollback-independent/sonuc.md)

Üç sayfa boyutu, iki kilitleme kipi ve karşılıklı C/Rust ile 480/480 sağlıklı geçiş C ile eş. 2.880 ana DB SHA ve 1.440 salt okunur açılış eş. Önceki yönlerden 360 koruma geçti. PERSIST sıfır başlık ve TRUNCATE sıfır boyut dosyasının normal kapanış ömrü C ile eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/wal-to-rollback-independent/res/proof.sha256)

Birleşik optimize adayda OFF 387/400 ve ON 400/400. Tam DML korpusunun ham özetleri V6b.94 ile aynı. [Tam korpus](/github/sqliters/target/perf-work/wal-to-memory-off-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,39 ms taban, 376,99 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-to-memory-off-disk-verify/res/gates-validation.txt).
