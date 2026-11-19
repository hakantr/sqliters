# V6b.173 — Salt okunur WAL çıkışında native kilit sonucu

Stock Unix VFS, gerçek salt okunur ana dosya, bağlı açık WAL-index oturumu ve etkin inode eşliğinde C'nin kilit sırasını kullanır. Linux'ta canlı okur veya yazarla `SQLITE_BUSY`, tek bağlantıyla gerçek ana dosya kilit isteğinden `SQLITE_IOERR_LOCK` alınır. Pager başarısız istekte SHARED ve WAL ilişkisini korur. V2 step, reset ve finalize kodları ve hata metinleri eşleşir. [Bağımsız native kanıt](/github/sqliters/target/perf-work/v171-readonly-wal-exit-lock-independent/sonuc.md)

120 temel profil, 32 okur sahipliği profili, 14 doğrudan stock, 14 legacy VFS ve 8 yazılabilir koruma profili ölçüldü. 714 C yeniden açılışı, 9.598 strict çıktı ve 20 C GCOV profili doğrulandı. [Strict manifest](/github/sqliters/target/perf-work/v171-readonly-wal-exit-lock-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/readonly-wal-exit-lock-legacy-final-release-proof/res/proof.sha256)

V1 ilk step ve bağlantı hata mesajı C gibi genel tanıyı verir. Reset alttaki 3850 kodunu korur. 48 profil ve 144 ayrı native C/OFF/ON akışında V1, V2 ve aktif işlem sınırları ölçüldü. [V1 bağımsız kanıt](/github/sqliters/target/perf-work/v173-readonly-wal-v1-mask-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 305,20 ms taban, 382,42 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/readonly-wal-exit-lock-legacy-disk-verify/res/gates-validation.txt).
