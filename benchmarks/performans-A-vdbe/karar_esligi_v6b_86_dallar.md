# V6b.86 — C indeks adı ve sahiplik kararları

C `sqlite3FindIndex` sonrası tabloya ait indeks listesi, PRIMARY KEY ve UNIQUE indeksine ilişkin adları içeriyor. Otomatik rowid alias'ın kalıcı indeksi bulunmuyor. 39 EXPLAIN/EQP ve 21 sıra/indeks koşulu karşılaştırıldı. Beş C gcov ölçümünde üç eşleşen ve iki eşleşmeyen indeks adı kararı gözlendi. [C dalları](/github/sqliters/target/perf-work/named-schema-index/sonuc.md)
