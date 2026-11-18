# V6b.66 — Örtük kapanış rollback bildirimi ve boş WAL-index başlığı

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Başlangıç V6b.65 `4de6c2c2a4796d3eee74ab9cd95cbc5b7fb80e7a`.

| C kararı | C sonucu | Rust karşılığı |
|---|---|---|
| `main.c:1404–1419` | `sqlite3_close_v2` son açık statement kapanana kadar temel bağlantıyı tutuyor. Son kapanışta btree'leri bırakmadan önce rollback yapıyor. | `ConnectionShared::drop` son handle kalmadığında btree rollback'ini tamamlıyor ve kayıtlı callback'i kapatmadan önce çağırıyor. |
| `main.c:1534–1539, 1556–1558` | Yazma transaction'ı varsa veya autocommit kapalıysa rollback hook bir kez çağrılıyor. Boş BEGIN ve okuma transaction'ı da autocommit koşuluna giriyor. | `Drop` mevcut `try_close` koşulunu ve panic izolasyonlu `invoke_rollback_hook` çağrısını kullanıyor. |
| `wal.c:1446–1453, 1604–1608, 974–986` | Boş WAL ilk kez okununca sıfır sayfa boyutu alanıyla iki geçerli SHM başlığı yayımlanıyor. | `scan_wal_file_for_recovery`, sıfır bayt dosya için `WalIndexHeaderData` sayfa boyutunu sıfırla kuruyor. Geçerli gerçek sayfa geometrisi ilk yazımda kullanılıyor. |

DELETE ve WAL için 10 olağan kapanış senaryosu ölçüldü. Normal `try_close`, örtük Drop, BUSY sonrası tekrar, bekleyen statement sonrası son kapanış ve açık ROLLBACK aynı callback sayısı ve son DB özetini verdi. 90 C/Rust yeniden açılışta commit edilmiş satır ve `integrity_check=ok` eşit. 28 ek guard vakası boş BEGIN, yalnız okuma, readonly bağlantı, autocommit, önceden ROLLBACK ve hooksuz akışı kapsadı. Hook ve SQL sonuçları ile ana DB özetleri eşit. İki Rust odak koşusu callback paniğinin taşmadığını, açık kapatmada reentry'nin `Misuse` döndüğünü ve son Drop'ta kapanan bağlantıya erişilemediğini doğruladı. Referans 3.54.1'in derlenmiş davranışı ve `main.c` kararı esas alındı.

512, 4096 ve 65536 baytlık sayfalarda, ilk okurun kapanması ve okurun açık kalmasıyla altı WAL akışı ölçüldü. İki Rust kipinde ilk salt okunur ve yazıcı açılışındaki boş SHM tam bayt eşit. Sonraki normal commit, FULL ve TRUNCATE sorgu izleri ile ana DB özetleri eşit. WAL başlığı ve frame checksum'ları bağımsız doğrulandı. Commit edilmiş WAL karşılaştırması bağımsız salt ve bağlı checksum alanlarını ayrı raporluyor. Sonraki TRUNCATE sonrasındaki iki eski SHM hücresi açık kaldı.

İki düzeltmenin birleşik 191 kaynak girdisi bağımsız adayda tekrar derlendi. 10/10 kapanış, 30 son DB, 90/90 yeniden açılış dosyası ve SQL sonucu ile 28/28 guard dosya kümesi eşit. Önce farklı olan 30 Rust WAL yeniden açılışı ile dört readonly WAL guard'ının SHM baytları birleşik adayda C ile birebir aynı. Ham dosya özetleri 429 kez yeniden hesaplandı. Açık transaction sırasındaki 40 karşılaştırmada journal nonce, WAL salt ve bunlara bağlı checksum'lar ayrı doğrulandı. Bu alanlar çıkarılınca journal, WAL ve SHM içerikleri eşleşti.

| Kapı | Sonuç |
|---|---|
| Çalışma alanı | OFF 1165 geçen, 6 başarısız, 337 yok sayılan. ON 1152 geçen, 19 başarısız, 337 yok sayılan. İki yeni regresyon testi geçti. Başarısız kümeler V6b.65 ile aynı. |
| DML fuzzer | OFF 380/400, ON 400/400. Ham özet V6b.65 ile aynı. |
| Ayrışma bulucu | 245/245 stdout, 244/245 stderr. Ham özet V6b.65 ile aynı. |
| Hız ve sonuç digest'i | 301,57 ms taban, 372,75 ms aday, 1,236 kat. 14/14 digest eşit. |

147/147 kurtarma, 12/12 imaj ve 4/4 gerçek C çöküşü kontrolü eşit. Kapı kayıtları `/home/hakan/github/sqliters/target/perf-work/close-empty-wal-verify/res` içinde.

Ham kanıt `/home/hakan/github/sqliters/target/perf-work/v6b66-close-combined-proof/res`, `/home/hakan/github/sqliters/target/perf-work/connection-close-proof/close-gcov` ve `/home/hakan/github/sqliters/target/perf-work/empty-wal-publish-proof/res` içinde. [Dal kapsamı](karar_esligi_v6b_66_dallar.md) · [Açık farklar](gorulen_farklar_v6b_66.md).
