# V6b.185 — Açık TEMP ile boş main WAL girişi

Açık TEMP ve bağlı dosya bulunmayan bağlantıda nitelemesiz `PRAGMA journal_mode=WAL`, boş ana NamedDisk'i mevcut koordineli statement sonuna alır. C gibi TEMP mevcut kip ve içeriğini korur, ana dosya ilk ROW boyunca sıfır bayt kalır. Sayfa 1, terminal yazıda ofset sıfırdan yazılır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v184-open-temp-empty-wal-staging-independent/sonuc.md)

912 profil ve 2.736 akışta 1.656 hedef fiziksel C–Rust çift eş. 1.024 rollback journal ve 632 journalsız çift, 24 canlı C eş profili ve 2.772 C yeniden açılışı doğrulandı. [Strict manifest](/github/sqliters/target/perf-work/v184-open-temp-empty-wal-staging-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/open-temp-empty-wal-staging-final-release-proof/res/proof.sha256)

Birleşik native tekrar 24 profil, 120 yazıcı yaşamı, 360 canlı C okuru ve 120 ayrı C açılışında geçti. [Birleşik fiziksel tekrar](/github/sqliters/target/perf-work/v185-combined-stage-replay.G9ZG8rGY/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 338,37 ms taban, 454,17 ms aday. Süre oranı 1,342 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/open-temp-empty-wal-staging-disk-verify/res/gates-validation.txt).
