# V6b.41 — E28 ve Pager ERROR

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_41_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:1996–2010,5772–5782`, pager_error çağrısında IOERR veya FULL | Pager ERROR durumuna girer ve tam genişletilmiş hata kodu saklanır. Getter aynı kodla döner. | `latch_pager_error`, `check_pager_error` ve page getter. DELETE FULL ve WAL IOERR spill testlerinde ikinci get ve sonraki yazma başlangıcı ilk hatayı döndürür. |
| `pager.c:4663–4760`, dirty sayfanın spill işlemi başarısız | Journal sync, DB write veya WAL frame hatası cacheflush çağrısına ve ERROR durumuna aktarılır. | `pager_stress`, `flush_unreferenced_dirty`. C cacheflush enjeksiyonları 13, 778, 1034 ve 778 kodlarını bir sonraki flush çağrısında korudu. Rust birim testi DELETE FULL ve WAL IOERR sonrası saklı kodu, cache bırakmayı ve sonraki yazıcıyı doğrular. |
| `pager.c:6723–6748`, faz bir DB/journal G/Ç hatası | Faz bir hata koduyla döner. B-tree geri alma yolu kullanılabilir. | `commit_phase_one` ve `rollback_inner`. Başarısız ilk ve üçüncü DB yazımı, journal/DB sync, FULL, WAL header/payload ve sync denetlendi. DELETE geri yazımları journal kaydı sırasını izler. |
| `pager.c:6764–6801,6838–6873`, faz iki veya geri alma G/Ç hatası | Faz iki sonlandırma hatası ya da rollback G/Ç hatası ERROR durumunu kurar. ERROR sırasında rollback veya commit tekrar çağrılırsa saklı hata döner. | `commit_phase_two` ve `rollback_retaining_read`. Doğrudan C pager probu DELETE 2570, TRUNCATE 1546, PERSIST 778/1034 için durum 6 ve tekrar çağrısında aynı kodu ölçtü. Rust, DELETE silme hatasında saklı kodu ve ikinci geri yazım hatasından sonra hot journal kurtarmasını doğruladı. |
| `pager.c:1890–1973,2240–2267,5533–5538`, ERROR ve son başvuru | Pager son referansa kadar hatayı tutar. Çıkışta cache geçersizleşir. Disk journal daha sonra kurtarılabilir. WAL okuma ve yazma kilitleri bırakılır. | `end_error_transaction` ve `unlock_error_if_unused`. İki Pager testi sayfa veya okuyucu ömrünü, epoch değişimini, yeniden açılışı ve sonraki yazıcıyı ölçer. Cache ve mmap lease'leri ayrı sayılır. |
| `pager.c:1941–1942,4102–4111,5329–5446`, ERROR çıkışında unlock ya da sonraki sıcak journal sync hatası | Başarısız unlock sonrası EXCLUSIVE tekrar edinilir. Sıcak journal DB'ye yazmadan önce sync edilir. Sync hatası journal'ı bir sonraki denemeye bırakır. | `error_lock_unknown` ve `play_hot_journal`. Silme, unlock, recovery sync hataları sırayla enjekte edildi. İkinci okuma eski satırı gördü. |
| `pager.c:2155–2158`, normal DELETE sonlandırması | Silme başarısız ve dosya yerindeyse sonraki okuyucu eski veriyi kurtarır. Silme tamamlandıysa sonraki dizin sync hatasına rağmen yeni veri kalır. | DELETE öncesi 2570 ve silme sonrası 1290 enjekte edildi. Canlı ve yeniden açılan bağlantıda sırasıyla eski ve yeni satır, son dosya hash'leriyle doğrulandı. |
| `btree.c:4445–4451`, `pager.c:1821–1825,2973–2977,4530–4543,5469–5488`, hatalı faz iki ve sıcak playback | Faz iki hata olarak dönerse sayaç artışı kalır. Sıcak playback cache'i atar. Sayfa 1 yazma denemesinde kaydedilen sürüm, diskten yeniden okunan sürümle karşılaştırılır. | Faz iki sayaç artışı, ilk sıcak kayıtta cache reset ve ham 16 bayt sürüm karşılaştırması C ile hizalandı. Altı DELETE akışında `PRAGMA data_version` aynı. Silme öncesi hata 1→5, silme sonrası 1→3, çift DB yazma hatası 1→4. Diğer üç faz bir hatasında 1→1. |
| `vdbeaux.c:3438–3447`, COMMIT hatası ve BUSY | Faz hatası işlemin geri alınmasını tetikler ve ilk hata raporlanır. Salt okunur COMMIT VDBE için BUSY tekrar edilebilir. | `pager_commit_failure` IOERR/FULL ile fiziksel geri almayı başlatır, hook'u bir kez çağırır ve ilk hatayı korur. BUSY yolu önceki davranışını sürdürür. |
| `vdbe.c:4071–4089`, üst düzey RELEASE ve hatalı Halt | B-tree işlemleri kapandıktan sonra SQL savepoint ve autocommit 0 geri gelir. | `release_savepoint_op`. DELETE ve WAL RELEASE vakalarında güncel veri, autocommit 0 ve kapanış hook'u karşılaştırıldı. |
| `main.c:1396–1443,1555–1557`, autocommit kapalıyken bağlantı kapanır | Etkin B-tree yazıcısı ya da açık SQL işlemi varsa rollback hook çağrılır. | `Connection::try_close`. Hatalı RELEASE sonrası kapanıştaki ikinci hook C ile eşit. |

Native matrisinde her iki anahtar kipinde 34 senaryonun 33'ü SQL/API sonucu, hata metni ve genişletilmiş kod, callback sırası, autocommit, canlı journal varlığı, yeniden açılan satırlar, `integrity_check` ve son dosya SHA-256 değerleri bakımından eşit. Önceki beş E28 COMMIT akışının tamamı eşit. Sekiz WAL senaryosunda yazma hatasının VFS ofseti veya çağrı boyutu farklı. Kalan farklar ve doğrulama sınırı ayrı kayıtta. [Görülen farklar](gorulen_farklar_v6b_41.md).

Ham kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b44/` altında. `res/matrix.json` hatanın VFS konumunu ve kalıcı dosya hash'lerini, `res/direct.txt` doğrudan C pager durumunu, `res/stress_c.txt` C cacheflush akışlarını ve `res/data_version.json` altı sayaç karşılaştırmasını içerir. Dört kapı ve kaynak kimliği `verified/res/` altında.

| Yayın kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1140 geçti, 7 başarısız, 337 yok sayıldı. ON 1126 geçti, 21 başarısız, 337 yok sayıldı. Önceki başarısızlıklar ve C ile çelişen F13–F15 kayıtlı. Y2 ve iki yeni pager testi iki kipte geçti. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.40 ile aynı. |
| Ayrışma bulucu | 245 stdout ve 244 stderr sonucu C ile eşit. V6b.40 ham özeti aynı. |
| Rust 1.100 tabanına hız, yedi deneme | Toplam medyan taban 300,97 ms, aday 372,97 ms. Aday/taban 1,239. 14 digest eşit. |

Bu doğrulama V6b.41 geliştirme kaydına aittir. Ürün yayını beklemede.
