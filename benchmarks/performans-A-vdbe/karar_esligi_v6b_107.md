# V6b.107 — Zorlanmış sırasız indeks taramasında reverse PRAGMA

`reverse_unordered_selects` açıkken doğal sırası istenmeyen, filtresiz ve açık INDEXED BY kullanan tek kaynak için mevcut fiziksel ikincil indeks cursor'ı ters yönde yürür. Aynı C bağlantısındaki ayar readback'i, normal `first/next` ve `last/prev` ayrımını doğruladı. Açık ORDER BY kendi yönünü kullanır. [C planı ve Rust akışı](/github/sqliters/target/perf-work/forced-reverse-pragma-independent/sonuc.md)

Her V1/V2 API ve kipte 198/198 fiziksel dosya, kalıcı satır ve C salt okunur açılışı eş. Ham eşlik OFF 60/198'den 171/198'e, ON 106/198'den 180/198'e çıktı. Önceki 177 IN grubunun bütün alanları aynı. Birleşik V107, V106 WAL checkpoint dalını içerir ve birleşik optimize DML önceki ham özetle eş, OFF 390/400, ON 400/400. [Odak manifesti](/github/sqliters/target/perf-work/forced-reverse-pragma-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/forced-reverse-pragma-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,11 ms taban, 374,90 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/forced-reverse-pragma-disk-verify/res/gates-validation.txt).
