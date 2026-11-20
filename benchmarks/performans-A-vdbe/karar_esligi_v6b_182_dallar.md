# V6b.182 — Hazırlık ve tekrar

148 ölçülen çiftte boş WAL ve SHM ilk step öncesine geçti. Doğal kilit çekişmesinde ayrı bağlantı olağan COMMIT yaptıktan sonra yeni hazırlık ve ilk ROW geçti. Gözlenen 138 WAL kapanışı, 30 takip yazması ve 152 önceki kapsam korumasında sözleşme doğrulandı. [Fiziksel denetim](/github/sqliters/target/perf-work/v180-readwrite-setter-prepare-independent/res/physical-audit.txt)
