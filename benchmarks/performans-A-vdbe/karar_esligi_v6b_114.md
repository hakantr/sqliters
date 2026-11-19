# V6b.114 — WITHOUT ROWID tablo adıyla indeks metadata

`index_info` ve `index_xinfo`, önce normal şema sırasıyla gerçek indeks adını arıyor. İndeks bulunamazsa ilk görünür WITHOUT ROWID tablonun gerçek birincil indeksini döndürüyor. Doğrudan ve tablo değerli çağrıda bağlı indeks önceliği ve MAIN tablo veya görünüm gölgelemesi C ile eş. [C arama sırası ve ham kayıt](/github/sqliters/target/perf-work/pragma-index-wr-fallback-independent/sonuc.md)

Üç kodlamada 72 metadata grubu, 456 düzeltilen sonuç bölümü ve 36 bayt eş boş dosya. C salt okunur açılış ve SHA kontrolleri geçti. Birleşik optimize DML OFF 390/400 ve ON 400/400, önceki ham özetle eş. [Odak manifesti](/github/sqliters/target/perf-work/pragma-index-wr-fallback-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/pragma-index-wr-fallback-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 296,55 ms taban, 374,36 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/pragma-index-wr-fallback-disk-verify/res/gates-validation.txt).
