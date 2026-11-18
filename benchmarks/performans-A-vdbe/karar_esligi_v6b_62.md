# V6b.62 — D166 normal WAL kapanışında VFS kararları

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_62_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `wal.c:2610–2624`, başarılı kapanış checkpoint'i | C, ana DB VFS'ine `SQLITE_FCNTL_PERSIST_WAL` sorgusunu `-1` ile gönderir. Tam `1` yan dosyaların korunması anlamına gelir. Sorgunun dönüş kodu hint sözleşmesine tabidir. | `sqliters-pager/src/wal.rs:2760–2796` normal DB kapanışında sorguyu yapar. `sqliters-vfs/src/lib.rs:217,2199–2211` dosya tanıtıcısına bağlı VFS ayarını ve mevcut adapter uyumluluğunu taşır. |
| `wal.c:2624–2647`, yan dosya seçimi | Kalıcılık değeri ve boyut sınırı WAL ve SHM dosya kararını belirler. | `wal.rs:2781–2808` mevcut silme ve sıfırlama işlemlerine sorgu sonucunu verir. |
| `wal.c:2610–2613`, `pager.c:4230–4285`, kapanış kilidi | BUSY halinde ara SHARED çağrısı yapılmadan kapanış sürer. Ana DB kapanışından önce terminal NO_LOCK gelir. | `wal.rs:2764–2775` ile `sqliters-pager/src/lib.rs:6425–6479` bu kararları normal boşta yazılabilir disk WAL kapanışında uygular. |

Kalıcı pager testi gerçek disk üzerinde varsayılan silme, normal kalıcılık ve VFS override için sorguyu, kapanış sırasını, checkpoint edilmiş DB'yi ve yeniden açılan veriyi denetliyor. [Açık farklar](gorulen_farklar_v6b_62.md).

Gerçek worktree'de 6/6 sağlıklı kapanış, 7/7 destekli VFS yanıtı ve 3/3 eski adapter vakasında kapanış ve yeniden açılış, bütünlük ve her aşamanın DB/WAL/SHM özetleri iki Rust kipinde C ile eşit. Ölçülen ortak VFS sırası eşit. Sağlıklı matris üç BUSY, altı başarılı EXCLUSIVE, altı kalıcılık sorgusu ve dokuz terminal kilit bırakma olayı içeriyor. Rust 1.100 odak pager ve WAL entegrasyon testleri toplam 8/8 geçti.

Ham VFS ve C kapsam dizini `target/perf-work/d166-proof/final-worktree/res/measurement-path.txt` üzerinden izlenir.

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1159 geçti, 7 başarısız, 337 yok sayıldı. ON 1145 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.61 ile aynı. Yeni kapanış testi iki kipte geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.61 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.61 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 298,07 ms, aday 370,07 ms, süre oranı 1,242.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/d166-verify/res` altında.
