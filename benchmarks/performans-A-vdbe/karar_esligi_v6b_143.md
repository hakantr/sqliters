# V6b.143 — Journalsız SQL geçişinin VFS bitişi

Normal NamedDisk SQL sayfa 1 geçişi fiziksel MEMORY veya OFF journal yolundaysa, başarılı bitişte C'nin COMMIT_PHASETWO bildirimi ve SHARED kilit bırakma sırası kullanılır. Ardından olağan NO_LOCK bitişi izlenir. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v141-journal-free-phase-two-independent/sonuc.md)

468 bitiş karşılaştırmasında VFS ve ham syscall sırası C ile eş. 168 önceki Rust bitişi ve 216 COMMIT_PHASETWO/SHARED çifti düzeldi. 5.256 DB SHA, 1.971 C salt okunur açılışı ve 38 C kapsam profili doğrulandı. [Strict fiziksel kanıt](/github/sqliters/target/perf-work/v141-journal-free-phase-two-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/journal-free-phase-two-platform-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 304,13 ms taban, 382,11 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/journal-free-phase-two-disk-verify/res/gates-validation.txt).
