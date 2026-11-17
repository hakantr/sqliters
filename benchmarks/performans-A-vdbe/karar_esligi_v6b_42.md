# V6b.42 — D145 EXCLUSIVE DELETE ve WAL yazma sınırları

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_42_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:2139–2158`, DELETE ve EXCLUSIVE | Commit veya rollback journal'ı soğutur. Başlık sıfırlanır ve açık tanıtıcı sonraki işlemde yeniden kullanılabilir. | `Pager::finalize_journal_after_tx` ve `zero_journal_header`. Normal ve EXCLUSIVE commit/rollback, 28 bayt başlığın yazma hatası ve takip eden sync hatası doğrulandı. |
| `pager.c:1436–1468`, journal_size_limit ve sync | Sıfır limit truncate yapar. Pozitif limit için önce başlık geçersizleşir ve sync edilir. Gerekirse sonra dosya kısaltılır. | `zero_journal_header`. 0, 1024 ve 20000 limitleri ile synchronous=OFF ve FULL akışları aynı. Silme veya kapanış sync hatası dosyayı bıraktığında kalan dosya baytları karşılaştırıldı. |
| `pager.c:2099–2104,2240–2250,4269–4273`, son okuyucu ve kapanış | NORMAL'a dönüşte son okuyucu tutulan journal'ı kaldırır. EXCLUSIVE bağlantı kapanırken journal önce sync edilir. Hata olursa journal dosyası kalabilir. | `release_retained_delete_journal` açık okuyucu snapshot'ını ve RESERVED veya daha yüksek kilidi denetler. xDelete ve xSync enjeksiyonu COMMIT/okuyucu sonu/kapanış sırasını doğruladı. |
| `pager.c:2932–2953`, sıcak nRec sıfır | Bölümde kayıt oynatılmaz. Eski, geçerli checksum'lı baytlar disk üzerinde kalmış olabilir. `0xffffffff` tüm bölüm kayıtlarını sayar. | `play_hot_journal`. Aynı disk imajı iki motorda 0, 1 ve `0xffffffff` nRec ile açıldı. Üçünde veri, integrity ve son dosya baytları eşit. |
| `vdbe.c:4180–4183`, SQL ROLLBACK ve pager hatası | Pager kodu iç katmanda kalır. Rollback hook autocommit henüz 0 iken çalışır. Ardından SQL işlemi kapanır. | `rollback_before_autocommit` iki yürütücünün ortak yoludur. EXCLUSIVE journal header ve sync hatalarında hook, SQL dönüşü, autocommit ve veri aynı. |
| `wal.c:4024–4065,4251–4258`, frame ekleme | Frame başlığı 24 bayt yazılır. Başarıdan sonra veri `off+24` adresine sayfa boyutunda yazılır. Padding sync sınırında yazım bölünebilir. | `Wal::write_frames` ve `write_with_sync_point`. 512, 1024, 4096, 8192 ve 65536 baytlık sayfalarla PSOW açık ve kapalı durumlarda WAL yazma boyutları, ofsetleri, sync ve VFS hata konumları eşit. |
| `wal.c:4075–4117,4233–4265`, açık işlemin önceki frame'ine yazma | Overwrite sayfa verisini yerinde yazar. Commit sırasında frame tek okumada alınır ve checksum için 24 baytlık başlık yeniden yazılır. | `wal_rewrite_checksums`. Aynı SQL sayfa imajlarıyla sekiz C/Rust akışında spill, overwrite, restart, padding, checksum başlığı ve kalıcı DB baytları eşit. |
| `wal.c:4281–4347`, sync ve görünürlük | Append ve gereken checksum yazımları sync'ten önce gelir. Hata sonrası WAL-index yayınlanmaz. Doğrudan xSync hatasında isteğe bağlı boyut sınırı yine denenir. | `write_frames`. Native WRITE, FULL ve FSYNC enjeksiyonlarında SQL sonucu ile kalıcı veriler aynı. Crash testleri ayrı header/payload yazımlarının disk üzerinde kalabilen alt kümelerini denetler. |

Her iki Rust kipinde 88/88 native vaka eşit. VFS hata konumları ve gözlenen WAL write/sync izleri de 88/88 eşit. Kalmaya zorlanan EXCLUSIVE journal dosyalarında, C VFS'de gözlenen journal checksum başlangıç baytları Rust girdisi olarak kullanıldı. Dosya hash'leri doğrudan, bayt ayıklamadan karşılaştırıldı.

Üç sayfa boyutu ve C, Rust OFF, Rust ON üreticileri için iki frame'in dört yazımından oluşan 144 WAL kesinti imajı kullanıldı. `-shm` olmadan aynı dosyalar her iki motorla açıldı. Dört yazımın tamamında yeni iki satır, diğer alt kümelerde eski iki satır kurtarıldı. SQL `integrity_check` ve açılış/kapanış sonrası ana dosya hash'leri aynı. Üç sıcak journal vakasıyla toplam 147/147.

Kanıt dizini `/home/hakan/github/sqliters/target/perf-work/v6b45/`. `res/matrix.json`, `res/rewrite.json`, `res/recovery.json`, `res/source_manifest.json` ve `res/oracle_and_binaries.json`. [Açık farklar](gorulen_farklar_v6b_42.md).

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1142 geçti, 7 başarısız, 337 yok sayıldı. ON 1128 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test kümeleri V6b.41 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.41 ile aynı. |
| Ayrışma bulucu | 245 stdout ve 244 stderr sonucu C ile eşit. V6b.41 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 297,95 ms, aday 371,62 ms. Aday/taban 1,247. 14 digest eşit. |

Gcov kapsamındaki 168 C dalının alınmayan 36 çıkışı ayrı kaydedildi.
