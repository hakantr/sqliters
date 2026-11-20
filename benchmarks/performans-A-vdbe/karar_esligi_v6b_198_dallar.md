# V6b.198 — Hazırlama, yürütme ve sınır

EXPLAIN ve prepare geçerli limiti koruyor. İlk ROW sırasında geçerli pozitif istek uygulanıyor. `128`, `0x80` ve `'0x80'` aynı sonucu veriyor. C'nin geçersiz dönüşüm saydığı alıntılanmış metinler, negatif ve sıfır sorguları önceki değeri koruyor. [Native vaka matrisi](/github/sqliters/target/perf-work/v197-max-page-count-independent/res/candidate/cases.json)
