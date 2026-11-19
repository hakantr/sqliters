# V6b.93 — C WAL geçiş commit'i ve önceki journal kipi

Ayrı C profillerinin her birinde 36 başlık geçişi ve 36 journal kapatma/silme kararı ölçüldü. Önceki kipteki ek commit, 18 PERSIST header-zero veya 18 TRUNCATE truncate kararı verdi. Ham ve yapısal dosya sınırları bağımsız raporda ayrı. [C dalları](/github/sqliters/target/perf-work/journal-persist-truncate-independent/sonuc.md)
