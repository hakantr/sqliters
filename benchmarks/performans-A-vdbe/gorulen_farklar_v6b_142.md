# V6b.142 — Açık kapsam

Kapsam normal rowid tablonun uygun doğrudan karma indeksli WHERE'siz UPDATE deyimi. Daha geniş DELETE, indeksli WHERE, fiziksel CREATE ve Rust REINDEX ayrı. Karşılaştırıcı doğrulamasında deyim başına tam tarama sürüyor. [Bağımsız kapsam](/github/sqliters/target/perf-work/v140-mixed-index-unfiltered-update.81uPRFBt/sonuc.md)
