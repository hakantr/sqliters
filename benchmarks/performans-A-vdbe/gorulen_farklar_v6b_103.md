# V6b.103 — Kapsam sınırları

Filtre, ANALYZE, DISTINCT, LIMIT, generated veya özel collation sütunları, ifade indeksi, geniş şema ve tam projeksiyonun ek ham plan farkları açık. Normal yazma odakları geçerli ve FK'siz. Arşivdeki 6189 ilk yazma vakası kapsam dışında tutuldu. [Kalan ham sonuçlar](/github/sqliters/target/perf-work/wr-unique-cover-independent/res/exact-audit.txt)
