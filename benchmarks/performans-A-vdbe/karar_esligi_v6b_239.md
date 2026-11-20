# V6b.239 — Görünüm UPDATE TEXT aralığı

Doğrudan tek rowid kaynaklı görünüm UPDATE işleminde tek tam BINARY TEXT indeks ve tek kanonik TEXT literal aralığı C'nin tarama ve tetikleyici sıra anahtarını izler. 75 hedef profilde 300 API, sıra ve nihai dosya çifti eş. Önceki sekiz TEXT farkı, geniş matriste 126 OFF ve 84 ON ilişkisel geri dönüş farkı kapandı. 160 ayrı koruma karşılaştırması ve V97/V109 korundu. 14.809 strict kayıt doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v237-view-update-text-range-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,06 ms taban, 376,26 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-text-range-disk-verify/res/gates-validation.txt).
