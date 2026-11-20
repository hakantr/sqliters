# V6b.241 — Görünüm UPDATE tırnaklı NUMERIC aralığı

Tek tam BINARY NUMERIC indeksli doğrudan rowid görünüm UPDATE işleminde mevcut affinity tırnaklı sınırı sayıya dönüştürünce aynı indeksli materyalizasyon kullanılır. 276 OFF ve 184 UTF16 ON eski kalıcı sıra/dosya farkı ile negatif sonsuzluktaki 20 ek fark kapandı. Önceki dört tırnaklı fark C eş. 4.380 native ve 4.380 C açılışı, 31.465 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v240-view-update-quoted-number-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 321,46 ms taban, 409,80 ms aday. Süre oranı 1,275 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-quoted-number-disk-verify/res/gates-validation.txt).
