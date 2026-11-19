# V6b.151 — MEMORY journal bağlantı kapanışında NO_LOCK

Normal NamedDisk ve açık fiziksel WAL veya journal olmadan MEMORY kipinde olan bağlantı, C gibi son db handle kapanışından önce terminal NO_LOCK çağrısını yapar. Açık son statement'ın işlemi bitirirken yaptığı önceki çağrı ayrı kalır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v149-terminal-no-lock-independent/sonuc.md)

378 MEMORY ve 648 normal koruma akışında VFS sırası eş. 216 eski Rust kapanış farkı ve 264 terminal çağrı giderildi. 9.408 C/Rust DB SHA, 4.032 C açılışı ve 2.244 gerçek ON izi doğrulandı. [Strict fiziksel kanıt](/github/sqliters/target/perf-work/v149-terminal-no-lock-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/terminal-no-lock-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,85 ms taban, 381,25 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/terminal-no-lock-disk-verify/res/gates-validation.txt).
