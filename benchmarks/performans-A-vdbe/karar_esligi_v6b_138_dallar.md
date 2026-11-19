# V6b.138 — OFF, MEMORY ve disk journal katılımcıları

OFF ve MEMORY başlangıçlı gerçek WAL geçişlerinde fiziksel rollback journal açılmadı. Karma DELETE, PERSIST ve TRUNCATE katılımcılarının ayrı journal yaşamı ve gerçek yazma sırası C ile karşılaştırıldı. Defensive OFF kararı 270 akışta korundu. [Fiziksel denetim](/github/sqliters/target/perf-work/v137-off-to-wal-independent/res/physical-audit.txt)
