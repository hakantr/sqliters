# V6b.115 — index_xinfo açık collation yazımı

CREATE INDEX anahtarında açıkça yazılan yerleşik COLLATE adı C'nin `index_xinfo.coll` metniyle eş. Katalog SQL'i ve mevcut sort-order türü metadata değerini belirliyor. Üç kodlamada 108 grup, 336 düzeltilen yazım ve 54 bayt eş boş dosya. Önceki 456 indeks arama kontrolü ve salt okunur C açılışları geçti. Birleşik DML OFF 390/400, ON 400/400, önceki ham özetle eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/index-xinfo-collation-independent/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/index-xinfo-collation-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,25 ms taban, 376,65 ms aday. Süre oranı 1,250 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/index-xinfo-collation-disk-verify/res/gates-validation.txt).
