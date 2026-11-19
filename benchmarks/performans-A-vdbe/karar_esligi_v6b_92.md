# V6b.92 — EXCLUSIVE DELETE→WAL journal sonlandırması

Geçişte başlık yazımı tamamlanana kadar rollback journal koruması sürüyor. EXCLUSIVE DELETE→WAL commit'inin ikinci aşaması, C'nin WAL hedefi için yaptığı gibi journal'ı mevcut silme yoluyla kaldırıyor. Normal DELETE commit'inde EXCLUSIVE journal tutma kararı korunuyor. [C kaynak ve bağımsız ölçüm](/github/sqliters/target/perf-work/journal-mode-normal-independent/sonuc.md)

512, 4096 ve 65536 bayt sayfalarda C→C, C→Rust ve Rust→C ile 240 sağlıklı geçiş ölçüldü. Taban 192/240, aday 240/240. 1.440 ana DB imajının SHA-256 değerleri eş ve 240 C salt okunur açılışı geçti. 54 olağan WAL RESTART ve 162 C yeniden açılışında fark çıkmadı. Rastgele journal ve WAL nonce/salt alanları, bunlara bağlı checksum doğrulamasından sonra yapısal karşılaştırmada ayrı tutuldu. [Bağımsız kanıt](/github/sqliters/target/perf-work/journal-mode-normal-independent/res/proof.sha256)

Birleşik optimize adayda OFF 387/400 ve ON 400/400. Tam DML korpusunun ham özetleri V6b.91 ile aynı. [Tam korpus](/github/sqliters/target/perf-work/exclusive-delete-wal-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 297,93 ms taban, 374,59 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/exclusive-delete-wal-disk-verify/res/gates-validation.txt).
