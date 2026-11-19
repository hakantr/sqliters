# V6b.105 — Kapsam sınırları

SQL düzeyinde eşit sıralama anahtarlarının kendi aralarındaki sıra tanımsız. Ölçüm C'nin doğal fiziksel indeks planını ve sağlıklı INSERT SELECT dosyasını kapsıyor. Gerçek sorter, kısmi sorter, alias, ordinal, özel NULL sırası ve ANALYZE ayrı kapsamda. İlk `reverse_unordered_selects` EXPLAIN arşivi PRAGMA'yı başka bağlantıda açtı. O iz varsayılan ayarı ölçer. Aynı bağlantı native satır kayıtları kendi ölçümlerini destekler. [Kapsam](/github/sqliters/target/perf-work/forced-order-direction-independent/res/exact-audit.txt)
