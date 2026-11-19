# V6b.106 — MAIN ve bağlı WAL checkpoint dalları

İki WAL ve karma DELETE/WAL, ayrı sayfa boyutları, PASSIVE/FULL/RESTART/TRUNCATE, nitelikli ve nitelemesiz PRAGMA ile native API ayrı ölçüldü. Eski okur kendi snapshot'ını korurken diğer dosya checkpoint edildi. 54 busy akışında tek sıfır indeksli callback, 18 PASSIVE akışında sıfır callback. [Dallar, açılışlar ve manifest](/github/sqliters/target/perf-work/v106-attached-checkpoint-independent/res/proof.sha256)
