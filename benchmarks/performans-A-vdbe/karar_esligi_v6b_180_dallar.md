# V6b.180 — Hazırlık, doğal çekişme ve kapanış

Checkpoint edilmiş WAL'ın 112 VFS çiftinde boş WAL ve SHM ilk step öncesi açıldı. Ayrı bağlantı olağan transaction'ı bitirince fresh prepare ve ilk ROW tekrar doğrulandı. Salt okunur, sıcak, ATTACH ve ordinary SELECT kontrolleri önceki akışı korudu. [Fiziksel denetim](/github/sqliters/target/perf-work/v178-readwrite-journal-query-prepare-independent/res/physical-audit.txt) · [Doğal BUSY koruması](/github/sqliters/target/perf-work/v178-readwrite-journal-query-prepare-independent/res/busy-guard.txt)
