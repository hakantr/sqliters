# V6b.76 — Açık CTE kararları

Ayrı lexical CTE tanımlarında farklı callback sırası ve kaynak tablosu bulunan flattening açık. Aynı FROM paylaşımı ve korelasyonsuz scalar UPDATE ömrü ölçülen şekillerde C ile eş. Gerçek dış sütuna doğrudan bağlanan CTE gövdesi OFF kipinde hâlâ tanı hatası veriyor. Bileşik veya karma FROM, bazı LIMIT callback sayıları ve iç trigger içindeki scalar CTE değerlendirmesi ayrı. [Kapsam ve C/Rust farkları](/github/sqliters/target/perf-work/cte-update-once/sonuc.md).
