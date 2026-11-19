# V6b.131 — Adlandırılmış disk ve geçici pager

Adlandırılmış disklerin TRUNCATE FULL kipinde 12 post-truncate fsync, NORMAL kipinde 12 sync yapılmayan akış öncekiyle eş. TEMP MEMORY, PERSIST ve DELETE güncel davranışları normal kontrol matrisinde korundu. [Ham fiziksel denetim](/github/sqliters/target/perf-work/v130-file-temp-truncate-independent/res/physical-audit.txt)
