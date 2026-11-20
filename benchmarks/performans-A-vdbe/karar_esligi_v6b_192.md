# V6b.192 — Bağlantı kapanış sonucu

`Connection::try_close` canlı statement veya handle nedeniyle kapanış kabul edilmezse mevcut BUSY sonucuyla bağlantıyı iade ediyor. Kapanış kabul edildiğinde C SQLite gibi kaynaklar kapatılıyor ve çağrı başarı dönüyor. Pager düzeyindeki temizleme sonuçları alt API’lerde görülebilir. [C bağlantı davranışı](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/c/sqlite3.c:189667) · [Bağımsız C ve Rust ölçümü](/github/sqliters/target/perf-work/v191-connection-close-result.dDXTiIo4/sonuc.md)

96 native profil, 192 C–Rust kapanış ve dosya çifti, 480 C yeniden açılışı. Dizin yan dosya silimine izin vermediğinde 18 eski SQL kapanış sonucu C ile hizalandı. 240 canlı statement korumasında BUSY ve aynı statement’ın daha sonra kullanılabilmesi doğrulandı. [Ham sayımlar](/github/sqliters/target/perf-work/v191-connection-close-result.dDXTiIo4/res/audit.txt) · [Exact kanıt](/github/sqliters/target/perf-work/v191-connection-close-result.dDXTiIo4/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 343,93 ms taban, 397,17 ms aday. Süre oranı 1,155 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/connection-close-result-disk-verify/res/gates-validation.txt).
