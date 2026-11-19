# V6b.153 — OFF journal bağlantı kapanışında NO_LOCK

Sağlıklı NORMAL kilitlemeli NamedDisk, açık disk journal ve WAL oturumu yokken mevcut OFF kipiyle kapanışta ana tanıtıcıdan hemen önce C gibi terminal NO_LOCK bildirir. Önceki okur transaction'ının bıraktığı NO_LOCK ayrı olaydır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v151-off-terminal-no-lock-independent/sonuc.md)

180 OFF ve 1.026 normal koruma akışı. 120 eski Rust kapanış farkı ve 132 terminal çağrı kapandı. 11.088 ana DB SHA çifti, 4.752 C salt okunur açılışı ve 2.974 gerçek ON izi doğrulandı. [29.380 strict kayıt](/github/sqliters/target/perf-work/v151-off-terminal-no-lock-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/off-terminal-no-lock-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,50 ms taban, 380,35 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/off-terminal-no-lock-disk-verify/res/gates-validation.txt).
