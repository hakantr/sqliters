# V6b.73 — Açık CHECK ve TEMP sınırları

İlgisiz sütunun UPDATE'i C'de önceki CHECK'i yeniden değerlendirmiyor. OFF altı ve ON UTF-16 dört vaka legacy yolda açık. V1'in step/reset hata maskesi ve önceki hata mesajı ayrı. ALTER, UPSERT, `CHECK(+v)` ve VIRTUAL BLOB kaynağı açık. [İlk kaynak ayrımı](/github/sqliters/target/perf-work/ignore-check-pragma-proof/sonuc.md) · [Ham yeniden ölçüm](/github/sqliters/target/perf-work/check-pragma-disk-reproof/res/verified.txt).

`PRAGMA lock_status` C'de DEBUG/TEST derlemesinde bulunur. Aynı source-id'li DEBUG karşılaştırması kullanıldı. Pinned normal C CLI için tanı PRAGMA arayüzü derleme özelliğine bağlı.
