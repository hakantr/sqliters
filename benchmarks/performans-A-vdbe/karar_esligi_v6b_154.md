# V6b.154 — Okuyucunun çağıran kesinti bayrağı

Mapped SELECT ve yorumlanan Read, çağıranın sağladığı AtomicBool açıkken aynı mevcut Interrupt denetimini kullanır. İlk satırdan önce Interrupt gelir, reset saklı sonucu taşır ve bayrak kapatılınca statement yeniden çalışır. C uyumlu varsayılan idle flag temizlemesi kendi yaşam testleriyle doğrulandı. [Kaynak ve ölçümler](/github/sqliters/target/perf-work/v152-custom-read-interrupt.g1AYa7Zq/sonuc.md)

Group46, yedi interrupt ve iki progress/pencere testi her kipte geçti. Bellek, DELETE ve WAL ile iki bağlantının ortak bayrak kontrolü iki kipte eş. Sekiz C salt okunur açılışı ve dört OFF/ON dosya SHA çifti eş. [Strict kanıt](/github/sqliters/target/perf-work/v152-custom-read-interrupt.g1AYa7Zq/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/custom-read-interrupt-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,66 ms taban, 377,78 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/custom-read-interrupt-disk-verify/res/gates-validation.txt).
