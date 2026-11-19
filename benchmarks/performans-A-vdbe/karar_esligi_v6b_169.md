# V6b.169 — Salt okunur journal_mode V1 dış tanısı

Normal salt okunur journal_mode çalışmasında V1 dış step, C gibi genel SQLITE_ERROR ve bağlantı tanısını bildirir. Asıl READONLY kodu mevcut statement yaşamında reset ve finalize için kalır. [C/Rust native ölçümü](/github/sqliters/target/perf-work/v167-journal-mode-v1-readonly.5wURfj29/sonuc.md)

DELETE→WAL için 24 C/Rust native karşılaştırması tam eş. WAL→DELETE V1 dış step 12 çiftte eş. 48 ana DB imajı ve 72 C salt okunur açılışı doğrulandı. [Strict kanıt](/github/sqliters/target/perf-work/v167-journal-mode-v1-readonly.5wURfj29/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/journal-mode-v1-readonly-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 311,54 ms taban, 394,05 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/journal-mode-v1-readonly-disk-verify/res/gates-validation.txt).
