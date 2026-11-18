# V6b.59 — D163 spill rollback geri yazma sınırı

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_59_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pager.c:2460–2467`, aktif ana journal geri yüklemesi | Ana DB yazımı için sync OFF veya kaydın sonunun `journalHdr` sınırında olması gerekir. Yazım kararı ayrıca dosya tanıtıcısını ve pager durumunu denetler. | `sqliters-pager/src/lib.rs:5686–5717` adı olan disk DB'nin aktif geri yüklemesinde kaydın son ofsetini kullanır. Sync açıkken sınır ötesindeki kayıt dosyaya yazılmaz. |
| `pager.c:2485–2514`, cache sayfası | Cache'de kalan sayfa eski içeriğini alır. Bu, dosyaya geri yazma kararından ayrıdır. | `lib.rs:5731–5754` bellekteki preimage'i cache için kullanmaya devam eder. Kalıcı test hem dosyayı hem tutulan cache sayfalarını denetler. |
| `pager.c:2461`, sync OFF | Ana journal için sync sınırı dosyaya geri yazmayı sınırlamaz. | `lib.rs:5706–5713` sync OFF halinde diğer geri yükleme koşullarını kullanır. Kalıcı test gerekli ikinci geri yazımı da sınar. |

Yeni pager testi gerçek spill, cache'de kalan sonraki sayfa, üç synchronous kipi, tam DB baytları ve cache geri yüklemesini denetliyor. [Açık farklar](gorulen_farklar_v6b_59.md).

Gerçek worktree'de 27/27 tek dosyalı vaka gözlenen VFS ve yazma sırası, sorgu, yeniden açılış ve ana DB özetlerinde iki Rust kipinde C ile eşit. NORMAL/FULL ile üç rollback kipinde önceki altı ek geri yazım kalktı. Sync OFF geri yazım dizisi korundu. Ham dosya eşliği 21/27. Altı PERSIST farkı fiziksel dosyalardan denetlenen nonce ve ona bağlı checksum alanlarıyla sınırlı. D162'nin 48 iki dosyalı ve sekiz guard açılışı vakası yeniden eşit.

Ham dosyalar, VFS ve C kapsam kayıtları `target/perf-work/d163-proof/final-worktree/res` altında.

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1156 geçti, 7 başarısız, 337 yok sayıldı. ON 1142 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.58 ile aynı. Yeni spill testi ve D162 entegrasyonu iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.58 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.58 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 298,84 ms, aday 373,49 ms, süre oranı 1,250.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/d163-verify/res` altında.
