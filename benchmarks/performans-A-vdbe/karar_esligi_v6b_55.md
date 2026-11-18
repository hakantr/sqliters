# V6b.55 — D159 normal WAL COMMIT phase-two bildirimi

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_55_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pager.c:2186–2191,2204–2205`, normal WAL COMMIT | WAL yazma işlemi sona erer. Başarılı COMMIT'te ana DB VFS'ine `SQLITE_FCNTL_COMMIT_PHASETWO` NULL ile gönderilir. | `sqliters-pager/src/lib.rs:5385–5420` normal NamedDisk WAL yazıcısını bırakır ve aynı bildirimi gönderir. |
| `pager.c:2206`, bildirim NOTFOUND | Tam NOTFOUND başarıya çevrilir. | `lib.rs:5452–5457` WAL ve rollback yollarında aynı kontrolü kullanır. |
| `pager.c:2204–2217,6799–6801`, bildirim hatası | Pager hata sonucu bildirir. WAL'a tamamlanmış commit kalıcıdır. Aktif okur ve sonraki açılışın gözlenebilir sonuçları korunur. | `lib.rs:5402–5417` commit edilmiş WAL durumunu korur ve okuyucuyu üst hata temizliğine bırakır. Kalıcı regresyon başarılı bildirim, NOTFOUND ve IOERR_WRITE ile ikinci bağlantıdan okuma sonucunu denetler. |

Doğal normal WAL COMMIT profili, NORMAL ve FULL ile etkin ve bırakılmış okuyucuları kapsıyor. Test VFS'inde aynı akış doğal NOTFOUND, açık OK ve IOERR_WRITE sonuçlarıyla karşılaştırılır. Sonraki sorgu ve yeniden açılışta okunabilir commit içeriği, DB özetleri ve WAL commit sayfası doğrulanır. [Açık farklar](gorulen_farklar_v6b_55.md).

Gerçek worktree'de 16/16 native vaka iki Rust kipinde C ile eşit. Bildirimden önce yazma kilidinin bırakılması, COMMIT sonucu, etkin okuyucu, sonraki sorgu ve yeniden açılış karşılaştırıldı. IOERR_WRITE vakalarında COMMIT hata verdi ve yeniden açılış commit edilmiş satırı okudu. 48 WAL görüntüsünde başlık ve frame checksum'ları bağımsız doğrulandı. Commit sayfası içerik özeti ve ana DB özetleri eşit. D155 rollback journal bildirimi ve dosya sonuçları 60/60 eşit.

Ham native kayıtlar ve kaynak özetleri `target/perf-work/v6b59-proof/final-worktree/res` altında.

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1152 geçti, 7 başarısız, 337 yok sayıldı. ON 1138 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.54 ile aynı. Yeni D159 testi ve D155 rollback regresyonu iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.54 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.54 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 298,22 ms, aday 370,30 ms, süre oranı 1,242.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/v6b59_verify/res` altında.
