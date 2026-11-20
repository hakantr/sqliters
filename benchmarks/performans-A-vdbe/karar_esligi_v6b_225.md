# V6b.225 — UTF-16 CAST BLOB AS TEXT

Geçerli UTF-16LE ve UTF-16BE kod birimleri gömülü NUL dahil korunuyor. Sondaki tek bayt C gibi atılıyor. 54 ayrı fark ve 24 fiziksel profil C ile eşleşti. 444 C açılışı ve 12 VDBE testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v225-utf16-cast-text-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 324,35 ms taban, 403,39 ms aday. Süre oranı 1,244 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/utf16-cast-text-nul-disk-verify/res/gates-validation.txt).
