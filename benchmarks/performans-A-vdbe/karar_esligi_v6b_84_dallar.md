# V6b.84 — C sanal cursor kararları

C EXPLAIN farklı sütunlara bakan OR kollarında aynı P1 ile iki VOpen yürütüyor. Taze C gcov kapalı ve açık cursor kontrolünün açık dalını 104 kez gözledi. Rust programı aynı cursor ve tablo kimliği olduğunda mevcut xOpen çağrısını koruyor. OR hazırlama callback sayısı 10'dan 5'e, IN sayısı 4'ten 2'ye indi. [C ve Rust kapsamı](/github/sqliters/target/perf-work/v84-vtab-or-in-independent/sonuc.md)
