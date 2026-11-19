# V6b.103 — Otomatik UNIQUE, PRIMARY KEY ve açık ikincil indeks

78 yürütme izi üç planı doğruladı. PK eke ihtiyaç duyan otomatik UNIQUE kapsama sayımı düzeltildi. Yalnız kendi anahtarını okuyan otomatik UNIQUE ve PK ekiyle kapsayan açık ikincil indeks kendi planlarını kullanıyor. UTF-8 yazma örneklerinde gerçek machine, UTF-16le ve UTF-16be örneklerinde ilişkisel fallback izlendi. [C maskeleri ve Rust izleri](/github/sqliters/target/perf-work/wr-unique-cover-independent/sonuc.md)
