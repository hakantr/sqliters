# V6b.134 — Açık kapsam

Fiziksel sıra denetimi deyim başına tablo ve indeksi tam tarıyor. Genel DML, diğer kolon yerleşimleri, fiziksel CREATE INDEX ve Rust REINDEX ile ayrı RTRIM/DESC ve WAL yazma kapsamları açık. [Bağımsız ölçüm ve sınırlar](/github/sqliters/target/perf-work/v132-mixed-index-rowid-dml-prototype.OnqsCXT9/sonuc.md)

WAL normal akışında gözlenen başlangıç CLI MAIN salt okunur ön açılış sayısı farkı açık. Asıl yazıcı açılışı ve normal commit/rollback/checkpoint gözlemleri eş. [WAL kapsamı](/github/sqliters/target/perf-work/v135-mixed-index-wal-dml.40QB95KX/sonuc.md)
