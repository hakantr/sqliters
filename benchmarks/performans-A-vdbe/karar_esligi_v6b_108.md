# V6b.108 — WITHOUT ROWID görünüm UPDATE sayısal aralığı

Tek MAIN kaynak, doğrudan görünüm projeksiyonu, tam açık ikincil indeks ve sayısal literal aralıkta kaynak satırlar C materyalizasyon sırasını izliyor. Mevcut range yürütmesi ve dış WHERE değerlendirmesi kullanılıyor. C ve Rust UPDATE INSTEAD OF logları ve fiziksel dosyalar eş. [C materyalizasyon ve Rust kaynak kanıtı](/github/sqliters/target/perf-work/view-update-wr-range-independent/sonuc.md)

Her V1/V2 ve kipte hedef 78/78 ham sonuç, kalıcı log, DB SHA ve C salt okunur açılışı eş. Önceki 138 rowid ve 54 WR kapsam kontrolü bütün alanlarıyla eş. Birleşik optimize DML ham özetleri öncekiyle birebir, OFF 390/400 ve ON 400/400. [Odak manifesti](/github/sqliters/target/perf-work/view-update-wr-range-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/view-update-wr-range-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,80 ms taban, 375,66 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-range-disk-verify/res/gates-validation.txt).
