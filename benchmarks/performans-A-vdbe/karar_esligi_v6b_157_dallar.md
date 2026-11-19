# V6b.157 — İlk hata ve statement kapanışı

C V1 ilk step SQLITE_ERROR, doğrudan reset/finalize SQLITE_BUSY döndürür. İkinci step sonrası gerçek saklı kod SQLITE_SCHEMA. V2 mevcut BUSY ve yeniden hazırlama akışını izler. [Native C ve Rust ham kayıt](/github/sqliters/target/perf-work/v154-journal-mode-v1-busy.UsINVVoT/res)
