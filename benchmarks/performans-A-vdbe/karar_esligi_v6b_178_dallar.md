# V6b.178 — Derleme ve reset yaşamı

Aynı bağlantı ALTER TABLE sonrası eski V1, yeniden çalıştırma ve cleanup kodlarında C ile eş. V2 otomatik hazırlamada erken expiry ve yürütmede saptanan şema değişikliği farklı mesaj aktarımı kullanıyor. Yeniden hazırlama olmadan ROW ve DONE gözlemleri mevcut tanıyı koruyor. [C ilk V2 adımlama izi](/github/sqliters/target/perf-work/v178-legacy-schema-expected.VkLWGFg8/v2-first/proof.sha256) · [V177'den birleşik yama ve kaynak](/github/sqliters/target/perf-work/v178-legacy-schema-expected.VkLWGFg8/combined-source-final/proof.sha256)
