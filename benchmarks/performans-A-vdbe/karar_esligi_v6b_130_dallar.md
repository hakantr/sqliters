# V6b.130 — Olağan, Drop ve gecikmeli kapanış

İki veya üç NamedDisk, iki ATTACH sırası, WAL ve DELETE, açık ve kapalı TEMP ile tek disk kontrolleri geçti. 468 gecikmeli serbest bırakmada son statement tamamlanana kadar disklerin baytları ve inode'ları korundu. Kapanış gerçek sona geldiğinde checkpoint ve unlink sırası C ile eş. [Strict SHA manifesti](/github/sqliters/target/perf-work/v128-close-slot-order-independent/res/proof.sha256)
