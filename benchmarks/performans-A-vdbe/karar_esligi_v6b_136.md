# V6b.136 — MEMORY→WAL statement bitiş sınırı

Normal NamedDisk MEMORY'den WAL'a geçerken C gibi dahili ara kipi kullanır. İlk ROW görünür WAL kipini bildirir, sayfa 1 (1,1) kalır ve bu katılımcı için fiziksel rollback journal açılmaz. DONE, reset veya finalize (2,2) başlığını yazar. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v133-memory-to-wal-independent/sonuc.md)

756 yeni ve 270 önceki kip korumasında sıfır fark ölçüldü. 432 erken ROW farkı kapandı. 19.008 DB SHA, 7.128 C salt okunur açılışı, 4.308 gerçek ON izi ve 22 C kapsam profili doğrulandı. [Strict native kanıtı](/github/sqliters/target/perf-work/v133-memory-to-wal-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/memory-to-wal-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 517,50 ms taban, 499,45 ms aday. Süre oranı 0,965 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/memory-to-wal-disk-verify/res/gates-validation.txt).
