# V6b.83 — Tek sanal tablo SELECT hazırlığında xBestIndex

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

UTF-8 ON tek tablo SELECT'in doğrulanabilir C hazırlığı aynı programı kullanıyor. Sessiz tanı araması tarafından elde edilen program uygun makine yolu için yeniden derlenmediğinde aynı C sorgusu ikinci `xBestIndex` çağrısını yapmıyor. İlk hazırlama veya yeniden hazırlamada plan ve yaşam süresi uygunluğu aynı kontrollerden geçiyor. `regexp()` yanlış arite tanısı ve retained V1 hata akışı korunuyor. [Kapsam ve kaynaklar](/github/sqliters/target/perf-work/v83-vtab-independent/sonuc.md).

132 bağımsız vaka ve 792 fiziksel dosya açılışı. Dosya SHA ve C yeniden açılış sonuçları iki kipte 132/132 eş. Çağrı sayısı değişen 32 UTF-8 ON vakasında C callback sırası ve argümanları eş. 31/32 ham tanı ve sonuç eş. Kalan V1 şema değişimi rc ayrı. 66 hazırlama koruması ve sekiz truth vakası ham tabana göre doğrulandı. Dump adresi ve sıra kontrolü tanısal karşılaştırma kapsamında. Bu ölçüm tam ham dump eşliği sayılmadı. [Bağımsız ham kanıt](/github/sqliters/target/perf-work/v83-vtab-independent/sonuc.md).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 383/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 297,94 ms taban, 377,38 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vtab-prepare-disk-verify/res/gates-validation.txt).
