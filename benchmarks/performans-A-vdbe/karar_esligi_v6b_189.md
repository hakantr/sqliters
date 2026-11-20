# V6b.189 — Tek hedefli WAL çıkışında busy handler

Normal kilitlemeli, yazılabilir tek disk hedefinde WAL kapatma hatası doğrudan dönüyor. C SQLite 3.54.1 soğuk SHARED, WAL açılışı ve EXCLUSIVE alma kararlarını `sqlite3PagerCloseWal` içinde veriyor. Sayfa 1 değişimi için daha sonra başlayan işlem mevcut yeniden deneme yolunu kullanıyor. [C kaynak kararı](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/c/sqlite3.c:66705) · [Bağımsız karşılaştırma](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/sonuc.md)

333 profil ve 999 C, Rust OFF, Rust ON yürütmesinde 498 hedef çift doğrulandı. 468 çiftte toplam 732 fazladan busy handler çağrısı ortadan kalktı. Başarısız adım, reset/finalize sonuçları ve eş bağlantı kapanınca başarılı yeni istek aynı. 48 hazırlık ve 36 WAL giriş kontrolü, gereken çağrıların korunduğunu gösterdi. [Bağımsız strict kayıt](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/res/proof.sha256)

Exact birleşik tekrarda 30 profil, 90 bağlantı akışı ve 90 salt okunur C yeniden açılışı tamamlandı. 54 yazılabilir C çifti ve altı salt okunur koruma gözlemi eşleşti. DML OFF 390/400 ve ON 400/400 özetleri önceki aşamayla byte eş. [Birleşik fiziksel tekrar](/github/sqliters/target/perf-work/v189-combined-stage-replay.letpcISe/proof.sha256) · [Birleşik DML](/github/sqliters/target/perf-work/single-wal-exit-busy-retry-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 307,44 ms taban, 387,92 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/single-wal-exit-busy-retry-disk-verify/res/gates-validation.txt).
