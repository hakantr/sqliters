# V6b.96 — C görünüm materialization ve DELETE sırası

C `sqlite3MaterializeView` koşulu geçici SELECT'e kopyalıyor. Temsilî aralıklı sorgunun EXPLAIN programında `SeekGE` ve `RowSetRead` sırası görüldü. Üç disk kodlamasında tetikleyici log sırası, rollback/commit verisi ve sonraki dosya yeniden açılışı karşılaştırıldı. [C kaynak ve dal kanıtı](/github/sqliters/target/perf-work/view-delete-order-independent/sonuc.md)
