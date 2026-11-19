# V6b.144 — Yabancı anahtar oldmask hazırlama tanısı

Uygun normal MAIN rowid parent UPDATE hazırlığı, C'nin seçili FK doğrulaması başarılıyken değişmeyen gelen FK sütunlarını da oldmask kararında çözümler. Uyumsuz şema tanısı ilk adımdan önce C ile aynı hata kodu, offset ve çocuk seçiminde üretilir. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v142-fk-prepare.Q2crMxBF/sonuc.md)

Yeni olağan şemalarda 18 önceki OFF hazırlama farkı kapandı. 1.080 native ve 540 CLI gözleminde 360 C/Rust çifti eş, 135 dosya SHA korundu. Önceki 882 geçerli FK deyimi ve 162 salt okunur açılış geçti. [Strict tanı kanıtı](/github/sqliters/target/perf-work/v142-fk-prepare.Q2crMxBF/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/fk-oldmask-prepare-platform-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 308,06 ms taban, 386,11 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/fk-oldmask-prepare-disk-verify/res/gates-validation.txt).
