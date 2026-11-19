# V6b.102 — C EXISTS iç indeks ve dış kaynak kararları

C EXISTS dönüşümündeki cursor bağımlılığı, iç indeks eşitliğini gerçekten kullandığı dış cursor sonrasına koyuyor. 61 C EXPLAIN ve 198 gerçek Rust plan izi, aynı ve farklı kaynak, dış rowid, örtülmeyen değer ve gerekli tablo taramasını ayrı gösterdi. [C kaynak ve dal kanıtı](/github/sqliters/target/perf-work/correlated-exists-covering-independent/sonuc.md)
