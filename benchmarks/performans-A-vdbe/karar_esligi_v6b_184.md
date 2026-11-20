# V6b.184 — Boş ana dosyanın tek hedefli WAL girişi

Tek hedefli WAL girişinde yeni veya sıfır bayt yazılabilir NamedDisk, normal kilitleme ile mevcut statement evreleme yordamını kullanır. C gibi ilk ROW boyunca ana dosya sıfır bayt kalır ve EXCLUSIVE tutulur. Sayfa 1, DONE, reset, finalize veya Drop sonlandırmasında tam sayfa boyutunda ve ofset sıfırdan yazılır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v182-empty-wal-entry-staging-independent/sonuc.md)

882 profil ve 2.646 akışta 1.308 C–Rust fiziksel çift eş. DELETE/PERSIST/TRUNCATE için 816 rollback journal çifti, MEMORY/OFF için 492 journalsız çift, aynı oturumda 240 okuma ve 2.646 C yeniden açılışı doğrulandı. [Strict manifest](/github/sqliters/target/perf-work/v182-empty-wal-entry-staging-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/empty-wal-entry-staging-final-release-proof/res/proof.sha256)

Birleşik native tekrar 12 profil, 60 yazıcı yaşamı, 180 canlı C okuru ve 60 ayrı C açılışı ile doğrulandı. [Birleşik fiziksel tekrar](/github/sqliters/target/perf-work/v184-combined-stage-replay.okKkmqL5/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,56 ms taban, 379,57 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/empty-wal-entry-staging-disk-verify/res/gates-validation.txt).
