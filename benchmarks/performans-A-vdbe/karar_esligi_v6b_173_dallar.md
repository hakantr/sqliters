# V6b.173 — Kilit sırası ve işlem sahipliği

Aynı süreçteki inode paylaşımı önce değerlendirilir. Ayrı süreç bağlantısında açık SHM oturumu üzerinden DMS sorgulanır. Sonraki ana dosya isteği mevcut SHARED aralığına gider. Ölçülen 180 ek akışta kurulu busy handler çağrılmadı. Açık bağlantıda takip okuması ve dosya imajı korundu. [Ham VFS ve fiziksel denetim](/github/sqliters/target/perf-work/v171-readonly-wal-exit-lock-independent/res/physical-audit.txt)
