# V6b.191 — Tek hedefli SQL WAL temizleme sonucu

Normal yazılabilir tek disk hedefinde C `sqlite3WalClose` kilit ve checkpoint sonucunu döndürüyor. SHM kapatma, WAL handle kapatma ve WAL silme işlemleri aynı sırada sürüyor. Rust SQL kipi değişimi aynı sonuç önceliğini kullanıyor. [C kaynak kararı](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/c/sqlite3.c:69461) · [Bağımsız karşılaştırma](/github/sqliters/target/perf-work/v189-wal-directory-cleanup.oViPrzq6/sonuc.md)

120 native profil, 120 izin kısıtlı ve 120 yazılabilir dizin C–Rust çifti. İlk ROW, DONE, finalize, autocommit, sonraki kip ve kullanıcı verisi eş. Ana DB, WAL ve SHM için 1.200 ham fiziksel faz eş. 144 journal fazında checksum doğrulandı ve bağımlı nonce alanları ayrıldı. 600 C salt okunur yeniden açılışı. [Bağımsız ham ölçüm](/github/sqliters/target/perf-work/v189-wal-directory-cleanup.oViPrzq6/res/audit.txt) · [Birleşik fiziksel tekrar](/github/sqliters/target/perf-work/v191-combined-wal-cleanup.vIXUQVlC/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 555,21 ms taban, 564,77 ms aday. Süre oranı 1,017 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/single-wal-cleanup-result-disk-verify/res/gates-validation.txt).
