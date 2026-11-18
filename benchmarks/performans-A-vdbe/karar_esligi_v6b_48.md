# V6b.48 — D152 sıcak journal kurtarmasında DB sync bildirimi

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_48_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:3031–3037,6462–6466`, başarılı sıcak playback | Ana dosyaya geri yazma tamamlanınca `SQLITE_FCNTL_SYNC` NULL argümanıyla çağrılır. Varsayılan Unix VFS NOTFOUND döndürür. | `VfsFile::notify_sync` (`sqliters-vfs/src/lib.rs:199–204`) aynı typed bildirimi tanımlar. Sıcak playback (`sqliters-pager/src/lib.rs:6497–6505`) `None` geçirir ve tam NOTFOUND dönüşünü başarı kabul eder. |
| `pager.c:6466–6471`, bildirim sonucu ve noSync | Başarılı bildirimde veya NOTFOUND sonrasında, synchronous etkinse fiziksel xSync yapılır. OFF durumunda bildirim gelir ve fiziksel xSync atlanır. | Aynı koşulda Rust `notify_sync` çağrısından sonra DB xSync yapar. OFF, NORMAL, FULL ve EXTRA için OK, açık NOTFOUND ve varsayılan VFS izlendi. |
| `pager.c:6464–6471,3034–3037`, G/Ç hatası | Bildirim IOERR verirse sonraki fiziksel xSync ve journal sonlandırması atlanır. Fiziksel xSync'in IOERR_FSYNC sonucu ayrı korunur. | `lib.rs:6499–6505`, bildirim IOERR 10 ve fiziksel IOERR_FSYNC 1034 sonuçlarını döndürür. İlk sorgu sonraki kurtarmaya aynı journal'ı bırakır. İkinci sorgu eski satırı döndürür. Kalıcı pager testi OFF durumunda geri dönüş ve tüm journal baytlarını da denetler. |

20 native sıcak DELETE vakasında iki Rust kipi C ile aynı DB ve journal VFS izlerini, iki sorgunun dönüş kodlarını ve her sorgu ile kapanıştan sonraki dosya özetlerini verdi. D151 bazında 20 vakanın tümünde bildirim izi eksikti. Bildirim IOERR olan dört vakada sorgu sonucu ve journal korunması da farklıydı. Ham ölçümler `/home/hakan/github/sqliters/target/perf-work/v6b52-proof/` altında. [Açık farklar](gorulen_farklar_v6b_48.md).

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1146 geçti, 7 başarısız, 337 yok sayıldı. ON 1132 geçti, 21 başarısız, 337 yok sayıldı. Yeni bildirim testi iki kipte geçti. Başarısız kümeler V6b.47 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.47 ile aynı. |
| Ayrışma bulucu | C ile 245 stdout ve 244 stderr sonucu eşit. V6b.47 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 299,25 ms, aday 372,22 ms. Aday/taban 1,244. 14 digest eşit. |

D151'in 24 sorgu ve dosya vakası ile sekiz cihaz yeteneği vakası korundu. Önceki 147 kurtarma, 12 journal imajı ve dört gerçek C çöküşü eşit. Dört kapı, ikili ve kaynak SHA-256 kayıtları `/home/hakan/github/sqliters/target/perf-work/v6b52_verify/` altında.
