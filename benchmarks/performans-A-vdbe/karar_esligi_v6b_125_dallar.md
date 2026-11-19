# V6b.125 — Tek, çoklu ve aynı kipte kalan hedefler

Tek PERSIST katılımcısında normal bitiş journal başlığını sıfırlıyor. Ortak journal kullanılan PERSIST ve TRUNCATE katılımcılarında dosya aynı inode üzerinde sıfır bayta iniyor. 288 aynı kip slotunun baytları ve inode'ları korundu. [Fiziksel denetim](/github/sqliters/target/perf-work/v124-multi-wal-to-retained-independent/res/physical-audit.txt)
