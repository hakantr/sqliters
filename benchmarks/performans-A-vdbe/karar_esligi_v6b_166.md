# V6b.166 — Koordineli OFF ve WAL BUSY sonrası kip

Mevcut OFF izni ile normal native bağlantıda önce tamamlanan OFF↔WAL slotu ilk gerçek okumaya kadar C gibi hedef kipi bildirir. Fiziksel geri alma ve normal yeniden okuma önceki güvenceyle çalışır. Nitelikli ve bütün slot tekrarları yeniden başlama sırasını korur. [Bağımsız C/Rust native kanıt](/github/sqliters/target/perf-work/v164-multi-busy-off-candidate.exVHfteY/sonuc.md)

156 OFF/WAL ve 108 DELETE/WAL koruma akışı, 528 son DB slot çifti ve 792 C açılışı eş. [3.199 strict kayıt](/github/sqliters/target/perf-work/v164-multi-busy-off-candidate.exVHfteY/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/multi-busy-off-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,61 ms taban, 377,21 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/multi-busy-off-disk-verify/res/gates-validation.txt).
