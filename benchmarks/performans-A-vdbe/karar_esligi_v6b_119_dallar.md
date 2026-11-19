# V6b.119 — DELETE/PERSIST/TRUNCATE hazırlık dalları

468 ilk ROW preimage ve checksum, PERSIST kuyruk ve mevcut inode, TRUNCATE yeni sayfa 1 ve mevcut inode, DELETE yeni journal ayrı. Normal commit'te bütün ileri sayfa 1 yazımları ilk unlink öncesinde. [Fiziksel denetim](/github/sqliters/target/perf-work/multi-retained-to-wal-independent/res/physical-audit.txt)
