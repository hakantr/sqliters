# V6b.128 — Açık kapsam

Normal bağlantı kapanışında C MAIN sonra AUX, Rust AUX sonra MAIN sırasını kullanıyor. Aynı diskin kapanış syscall sırası ve ana DB baytları eş. Sonraki olağan TEMP FILE TRUNCATE yazılarında sync sınırı ayrı doğrulama gerektiriyor. [Kaynak ve açık kapsam](/github/sqliters/target/perf-work/v126-open-temp-journal-mode-independent/sonuc.md)
