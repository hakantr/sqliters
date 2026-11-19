# V6b.131 — Açık kapsam

FILE TEMP dosya ve journal tamponlama, anonim geçici dosya açılışı ve yazma gruplaması farklılıkları açık. NamedDisk PERSIST başlığında C fsync ve Rust fdatasync farkı ayrı ölçüldü. Cache içi NamedDisk rollback için C ek pread yapıyor. [Kaynak ve ham karşılaştırma](/github/sqliters/target/perf-work/v130-file-temp-truncate-independent/sonuc.md)
