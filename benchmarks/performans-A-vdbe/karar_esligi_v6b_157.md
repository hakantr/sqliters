# V6b.157 — V1 journal_mode kilit ve yeniden adım

V1 journal_mode step, C gibi BUSY ve SCHEMA için genel SQLITE_ERROR bildirir. Gerçek kod reset ve finalize'a taşınır. İlk yürütmede yapılan expiration, ikinci step ile eski deyimin yeni dosya kipi yazmasını önler. [C kaynak sırası ve bağımsız ölçüm](/github/sqliters/target/perf-work/v154-journal-mode-v1-busy.UsINVVoT/sonuc.md)

72 doğal kilit çekişmesi ve 24 sağlıklı pragma grubu. 192 native C/Rust ve tam DB dosyası çifti eş. 216 C salt okunur açılışta veri ve bütünlük doğrulandı. [Strict kanıt](/github/sqliters/target/perf-work/v154-journal-mode-v1-busy.UsINVVoT/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/journal-mode-v1-busy-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,36 ms taban, 376,38 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/journal-mode-v1-busy-disk-verify/res/gates-validation.txt).
