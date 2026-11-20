# V6b.185 — TEMP ve ana dosya sahipliği

TEMP FILE, DELETE ve TEMP MEMORY, MEMORY kipini korur. 144 aynı oturum okuması ve ayrı C okurun ROW sırasında doğal BUSY durumu eş. Super-journal açılmadı. V128'in önceki TEMP WAL çıkışları ve V141'in retained WAL/SHM inode koşulları ayrıca doğrulandı. [Fiziksel ve sahiplik denetimi](/github/sqliters/target/perf-work/v184-open-temp-empty-wal-staging-independent/res/physical-audit.txt)
