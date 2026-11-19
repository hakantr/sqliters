# V6b.171 — Hazırlık ve dosya sahipliği

Soğuk salt okunur WAL sorgusu gerekli SHARED ve WAL hazırlığını ilk ROW öncesinde tamamlar. Dört soğuk salt okunur DELETE karşılaştırmasında C'nin SHARED→NO_LOCK sınırı doğrulandı. Sıcak bağlantılar, yazılabilir bağlantılar ve diğer mevcut journal kiplerinin 246 kontrol akışı korundu. [Fiziksel denetim](/github/sqliters/target/perf-work/v166-readonly-journal-query-prepare-independent/res/physical-audit.txt)
