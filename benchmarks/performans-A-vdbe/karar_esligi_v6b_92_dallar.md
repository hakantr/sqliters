# V6b.92 — C WAL geçişinde journal kaldırma kararı

`OP_JournalMode` başlık işlemini rollback journal ile korur ve sonlandırma sırasında hedef WAL seçilidir. `pager_end_transaction`, eski EXCLUSIVE DELETE işlemindeki sıfır başlık bırakma davranışı yerine journal kapatma ve silme kararına girer. Bağımsız C çalışmasındaki commit ve kapanış sınırları raporda izlenir. [C kaynak ve dosya kapsamı](/github/sqliters/target/perf-work/journal-mode-normal-independent/sonuc.md)
