# V6b.194 — Ortak indeks nesli ve ilk append

Doğal küçük-cache spill, açık savepoint'ten sonra C WAL neslini değiştirdi. İç geri alma ilk neslin frame konumunu sıfırda kabul etti. Canlı READ0 snapshot, dış commit veya rollback ve daha sonraki bağımsız yazıcı C ile aynı veriyi ve aynı değişim evresini gördü. [C dal sayımı](/github/sqliters/target/perf-work/v193-restart-spill-savepoint-independent/res/c-coverage/sqlite3.c.gcov:78931) · [Bağlantı yaşamı](/github/sqliters/target/perf-work/v193-restart-spill-savepoint-independent/res/lifecycle/cases.json)
