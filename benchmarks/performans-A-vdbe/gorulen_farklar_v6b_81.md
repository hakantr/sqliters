# V6b.81 — Açık WITHOUT ROWID plan kararları

Üretilen STORED sütun içermeyen tabloların sırasız tam indeks kararı eski yolda. ANALYZE sonrasındaki sıra, NOT INDEXED ve implicit PK'yi zorlayan INDEXED BY için eski farklı davranışlar açık. Son dar adayın kapsamı mevcut release çıktısı ve fiziksel DB eşliğine göre seçildi. [Açık kalan farklar](/github/sqliters/target/perf-work/without-rowid-stored-scope/sonuc.md).
