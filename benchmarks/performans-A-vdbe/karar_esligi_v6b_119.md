# V6b.119 — Çoklu retained journal ve ortak WAL girişi

İki ve üç NORMAL disk hedefinde DELETE, PERSIST ve TRUNCATE rollback kipleri WAL girişinin mevcut ortak hazırlık ve commit akışına katılıyor. İlk ROW journal biçimi, inode, ters hazırlık ve ileri sayfa 1 yazımı C eş. [Kaynak, dosya ve syscall kanıtı](/github/sqliters/target/perf-work/multi-retained-to-wal-independent/sonuc.md)

180 yeni sağlıklı akış ve 612 önceki kontrol geçti. Sekiz karma PERSIST yazım sırası farkı kapandı. 11.700 ana SHA ve 3.528 C salt okunur açılışı doğrulandı. Birleşik optimize DML OFF 390/400, ON 400/400 ve önceki ham özetle aynı. [Odak manifesti](/github/sqliters/target/perf-work/multi-retained-to-wal-independent/res/proof.sha256) · [Birleşik korpus](/github/sqliters/target/perf-work/multi-retained-to-wal-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,82 ms taban, 378,85 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/multi-retained-to-wal-disk-verify/res/gates-validation.txt).
