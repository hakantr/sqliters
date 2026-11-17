# V6b.34 — görülen farklar ve açık kapsam

- D142'nin önceki altı etkin okuyucu farkı kapandı. Genişletilmiş normal Unix akışlarında aynı sayfada iki okuyucu, normal/mapped ref karışımı, EOF256, dar mmap, shrink, sayaçlardan ayrılık, WAL frame, TEMP, bağlı şema, sıralama, reset/finalize ve rollback denetlendi. Genel legacy planlar ve özel VFS'ler için bu kapsamın dışında eşlik kanıtı gerekiyor.
- İlk eşlemeden sonra dosya büyümesinde bulunan 36 D142 farkı SIZE_HINT ile kapandı. Rollback journal ve WAL checkpoint dahil 52/52 büyüme/geometri akışı eşit. 17 doğrudan VFS adımı aktif ref guardını doğrular. Özel chunk-size, mremap syscall sırası ve NOMEM/IOERR kapsaması açık.
- D144 önceki dört CREATE TABLE hata ofseti. C 13 ve Rust -1. Ham sonuçları değişmeden duruyor.
- Genişletilmiş matriste yedi eski fark var. OFF'ta bulunamayan `nope` sütunu için C offset 7 ve Rust -1. Kesik veritabanı dört akışta iki farklı CORRUPT iletisi üretiyor. Bozuk hücrede `cell_size_check` kapalıyken iki akışta C ilk satırı `13|NULL` döndürüyor ve Rust CORRUPT veriyor. Akışların ham C ve Rust çıktıları V6b.33 başlangıç çalıştırmasıyla aynı.
- Anonim TEMP dosyasının ham bayt dökümü, mmap başarısızlığının `sqlite3_log` uyarısı, mremap, Windows xFetch, fstat/xFetch/WAL hata enjeksiyonu, NOMEM, sorter spill I/O ve E28 pager ERROR açık.
- D143/D57 modül envanteri ve M4 DDL yürütümü devam ediyor. Tam yayın için eski fail kümeleri, finder ve DML sapmaları ile önceki raporlarda açık bulunan veri/dosya eşliği kusurları sürüyor. [Ölçülen kararlar](karar_esligi_v6b_34.md).
