# V6b.227 — auto zaman değiştiricisi

İlk auto, ham sayıyı C’nin Julian veya Unix sınırlarına göre yorumluyor. 1.125 ana girdide 687 ayrı ve önceki zaman matrisinde 28 ayrı fark kapandı. 540 C açılışı, ilgili kodlama ve 12 VDBE testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v226-auto-modifier-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 305,72 ms taban, 384,13 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/datetime-auto-modifier-disk-verify/res/gates-validation.txt).
