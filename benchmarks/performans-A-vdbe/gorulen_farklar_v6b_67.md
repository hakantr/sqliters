# V6b.67 — Açık SQL eşlik maddeleri

Normal SQL koşullarında 2160/2160 ve REGEXP matrisinde 576/576 eşit. Ayrı `IS TRUE/FALSE` gramer ve opcode kontrolünde 480 ifadenin önceki ham sonuçları aynı kaldı. OFF 204 ve ON 136 eski fark var. Zorlanan UTF-16 C VDBE'de BLOB döndüren built-in işlevin hücre kodlamasına ilişkin 30 fark da önceki kaynakla aynı.

Üç kodlamada sondaki sayısal BLOB `CHECK` örnekleri ayrı bir yazma ve doğrulama farkı ortaya çıkardı. Legacy INSERT/UPDATE uygun satırı atlıyor. UTF-8 ON yazımı başarılı olduğunda aynı satırı legacy `integrity_check` yanlışlıkla CHECK hatası olarak gösteriyor. `ALTER TABLE ADD COLUMN DEFAULT` içindeki BLOB kodlaması da ayrı C kararına bağlı. [CHECK ham kayıtları](/home/hakan/github/sqliters/target/perf-work/check-blob-parity/res/cli-results.json) · [ALTER ham kayıtları](/home/hakan/github/sqliters/target/perf-work/check-blob-parity/res/alter-matrix.json).

Doğrudan sayısal dönüşün hata kodu ve bağımsız built-in dispatch yolları ayrı doğrulama gerektiriyor. Kapanış/SHM açıkları V6b.66'da. [Karar ve doğrulama](karar_esligi_v6b_67.md) · [V6b.66 açıkları](gorulen_farklar_v6b_66.md).
