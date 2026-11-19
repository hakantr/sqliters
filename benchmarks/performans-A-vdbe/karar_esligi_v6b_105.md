# V6b.105 — Zorlanmış indeksin sağladığı ORDER BY yönü

Yalın ve filtresiz tek kaynakta açık INDEXED BY seçiminin bütün ORDER BY terimlerini karşıladığı doğrulanıyor. C aynı ikincil indeksin geri yönünü seçince mevcut cursor `last/prev` kullanıyor. Açık WITHOUT ROWID ikincil indekste fiziksel PK eki tam sıralama kararına katılabiliyor. [Kaynak ve yön kanıtı](/github/sqliters/target/perf-work/forced-order-direction-independent/sonuc.md)

Her V1/V2 API ve kipte 195/195 dosya, kalıcı satır ve C salt okunur açılışı eş. Ham sonuç OFF 63/195'ten 156/195'e, ON 107/195'ten 169/195'e çıktı. Önceki 177 IN grubunun bütün alanları birebir. Önceki 219 fiziksel indeks kontrolü korunurken iki bağ sırası ölçümü eşleşti. Birleşik optimize DML OFF 390/400 ve ON 400/400, ham korpus özetleri aynı. [Odak manifesti](/github/sqliters/target/perf-work/forced-order-direction-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/forced-order-direction-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,90 ms taban, 377,14 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/forced-order-direction-disk-verify/res/gates-validation.txt).
