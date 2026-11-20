# V6b.190 — Açık kapsam

Birden fazla ATTACH ile koordineli WAL çıkışı ayrı kapsam. Native C SHM DMS ön kontrolü ve Rust ana dosya kilit denemesi ham sistem çağrısı düzeyinde farklı. 108 salt okunur ve tek hedef çifti ile 60 V187 WAL giriş profili önceki davranışı korudu. V161/V164 arşivinin 72 profilinde iki eski giriş ROW dosya farkı açık. [Bağımsız rapor](/github/sqliters/target/perf-work/v189-coordinated-wal-exit-busy-independent/sonuc.md) · [Önceki geçiş korumaları](/github/sqliters/target/perf-work/v189-coordinated-wal-exit-busy-independent/res/v161-v164-guard.txt)
