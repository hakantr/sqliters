# V6b.47 — D151 sıcak PERSIST ve TRUNCATE journal sonlandırması

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_47_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:2124–2138`, TRUNCATE | Başlık ilerlediyse açık tanıtıcıyla xTruncate(0) yapılır. FULL ve EXTRA'da başarılı kesme sonrasında xSync yapılır. Başlık ilerlemediyse kesme çağrısı yok. | `finish_hot_journal` (`lib.rs:6690`) aynı tanıtıcıyı `finalize_journal_after_tx` (`lib.rs:5763`) yordamına verir. Dört synchronous kipi, kesme hatası ve kısa veya geçersiz başlık ölçüldü. |
| `pager.c:2139–2143,1436–1468`, PERSIST | Başlık ilerlediyse ilk 28 bayt sıfırlanır. Super-journal veya sıfır limit dosyayı tamamen keser. Pozitif limit sync sonrasında uygulanır. | `finalize_journal_after_tx` ve `zero_journal_header` açık journal'ı sonlandırır. Boyutlar, ilk 28 bayt, DATAONLY sync, limitler, super-journal ve xWrite hatası ölçüldü. |
| `pager.c:1915–1940`, NORMAL kilit bırakma | Unix VFS journal tanıtıcısını NO_LOCK öncesinde kapatır. `UNDELETABLE_WHEN_OPEN` açık PERSIST/TRUNCATE tanıtıcısını korur. | `close_journal_before_unlock` (`lib.rs:4962`) başarı ve ERROR yollarında aynı cihaz seçimini uygular. 4/4 tutulan tanıtıcı ve 4/4 standart kontrol, iki sorgudaki açma ve kapatma izleri ve tüm dosya özetleriyle eşit. |
| `pager.c:2209–2217,3036–3046,5425–5443`, sonlandırma ve hata | Journal sonlandırma hatası SHARED girişimini engellemez. İlk hata döner ve super-journal temizliği ancak iki işlem de başarılıysa yürür. | `play_hot_journal` (`lib.rs:6448`) sonlandırma ve SHARED sonucunu C sırasıyla alır. PERSIST 778 ve TRUNCATE 1546 hata vakalarında ilk journal ve ikinci sorgudaki kurtarma eşit. |
| `pager.c:2903–2908,3036–3046,1440,2125`, commit noktası geçilmiş | Geçerli trailer'daki super-journal dosyası yoksa sayfa playback'i atlanır. Sıfır journalOff ile PERSIST ve TRUNCATE dosya baytlarına dokunmaz. DELETE dosyayı kaldırır. | `lib.rs:6482–6505,6690–6707` aynı koşulda boyutu veya başlığı değiştirmez. C, OFF ve ON üç kipte bağlı dosya mevcut ve yok vakalarını eşledi. |

Önceden PERSIST/TRUNCATE ayarlanmış 22 vaka ve PRAGMA'nın ilk komut olduğu iki kontrol, iki sorgunun VFS izinde ve bütün ara ve son dosya özetlerinde C ile eşit. Ham kayıtlarda C'nin bağlantı kapanışındaki NO_LOCK çağrısını ayrıca görürüz. Ek cihaz yeteneği matrisinde 8/8 sorgu ve dosya sonucu eşit. Normal ve EXCLUSIVE tanıklarının ayrıntıları, terminal bağlantı close farkları ve açık commit SHARED farkı ayrıca kayıtlıdır. [Açık farklar](gorulen_farklar_v6b_47.md).

İlk çalışma alanı koşusunda iki kipte de eski PERSIST sıfır uzunluk beklentisi ayrıştı. C'nin ürettiği gerçek 2PC commit görüntüsünde C, OFF ve ON iki DB açılış sırasında da committed satırları döndürdü ve her iki journal'ı bayt bayt korudu. Ayrı altı vakada sorgu izleri ve ara ve son dosya özetleri eşit. Super-journal mevcut PERSIST ve TRUNCATE karşı kontrollerinde rollback gerçekleşti ve dosya kesildi. `tests/phase2_super_journal_2pc.rs` artık PERSIST journal'ın bütün baytlarını karşılaştırıyor. DELETE, satır ve bütünlük kontrolleri korunuyor. İlk koşu kayıtları `/home/hakan/github/sqliters/target/perf-work/v6b51_verify/res/initial_tests_off.txt` ve `initial_tests_on.txt` içinde. Ham C karşılaştırmaları `/home/hakan/github/sqliters/target/perf-work/v6b51-proof/commit-point/res/` ve `/home/hakan/github/sqliters/target/perf-work/v6b51-missing-review/` altında.

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | Düzeltilen 2PC testi ve genişletilen pager testi iki kipte geçti. OFF 1145 geçti, 7 başarısız, 337 yok sayıldı. ON 1131 geçti, 21 başarısız, 337 yok sayıldı. Başarısız kümeler V6b.46 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.46 ile aynı. |
| Ayrışma bulucu | C ile 245 stdout ve 244 stderr sonucu eşit. V6b.46 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 298,43 ms, aday 369,51 ms. Aday/taban 1,238. 14 digest eşit. |

D149 başlık vakaları 203/203, D150 sonlandırma vakaları 40/40 ve önceki 147 kurtarma, 12 journal imajı ile dört gerçek C çöküşü eşit. Test düzeltmesinden sonra çalışma alanı iki kipte yeniden çalıştı. DML, ayrışma bulucu ve hız kapıları aynı SHA-256 değerindeki üretim kaynağına ait. Ham kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b51_verify/res/` ve `/home/hakan/github/sqliters/target/perf-work/v6b51-proof/final-worktree/res/` altında.
