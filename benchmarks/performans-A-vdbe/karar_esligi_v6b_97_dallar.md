# V6b.97 — C görünüm UPDATE materialization sırası

C geçici taramanın satır sırasını INSTEAD OF ve alt AFTER tetikleyicisine geçiriyor. Üç disk kodlamasında gerçek UTF-8 makine ile iki UTF-16 fallback yolu ölçüldü. Sıra lehte ve ters indeks yönlerinde, nested yazım dahil, kalıcı log ve ana tabloyla doğrulandı. [C kararları](/github/sqliters/target/perf-work/view-update-order-independent/sonuc.md)
