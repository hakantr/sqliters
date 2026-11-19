# V6b.162 — Salt okunur WAL terminal NO_LOCK

Normal salt okunur NamedDisk WAL kapanışı, ana tanıtıcı kapanmadan önce C gibi terminal NO_LOCK bildirir. Yazılabilir checkpoint kararı aynı önceki koşulla seçilir. [Bağımsız C ve Rust kaydı](/github/sqliters/target/perf-work/v159-readonly-wal-close-independent/sonuc.md)

432 salt okunur ve 126 normal guard akışında 288 terminal bildirim hizalandı. 2.112 ana DB SHA çifti ve 960 C salt okunur açılışı eş. [17.779 strict kayıt](/github/sqliters/target/perf-work/v159-readonly-wal-close-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/readonly-wal-terminal-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 309,08 ms taban, 387,14 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/readonly-wal-terminal-disk-verify/res/gates-validation.txt).
