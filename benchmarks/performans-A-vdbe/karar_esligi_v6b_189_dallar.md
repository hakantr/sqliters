# V6b.189 — Kilit ve yeniden deneme evreleri

Soğuk girişte C doğrudan SHARED alıp WAL dosyasının varlığını sorgular. WAL varsa açar ve EXCLUSIVE almaya çalışır. EXCLUSIVE başarısızsa eski kilit düzeyini geri koyar. Bu evrenin hata sonucu çağırana gider. WAL bulunmadığında sayfa 1 işlemi için gereken yeniden deneme önceki davranışı izler. [C WAL kapatma akışı](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/c/sqlite3.c:66705) · [C dal gözlemleri](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/res/c-wal-close-branches.txt)

DELETE, PERSIST, TRUNCATE, MEMORY ve OFF hedefleri, soğuk/sıcak hazırlık, V1/V2 ve 512, 4096, 65536 sayfa boyutları ölçüldü. Fiziksel takım, ilk başarısız adım ve başarılı tekrar aynı kayda bağlandı. [Fiziksel denetim](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/res/physical-audit.txt)
