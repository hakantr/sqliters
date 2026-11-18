# V6b.52 — D156 disk rollback-journal kayıt yazımı

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_52_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pager.c:6149–6177,6076`, ilk mevcut sayfa yazımı | Sayfa journal açıldıktan sonra dirty, kayıt yazımından önce NEED_SYNC olur. Yazım başarısızsa NEED_SYNC korunur ve sayfa yazılabilir duruma geçirilmez. | `sqliters-pager/src/lib.rs:4000–4007,4078–4099`. Test her kayıt parçasında IOERR_WRITE vererek dosya boyunu, bayrakları ve sonraki açılışı denetler. |
| `pager.c:6078–6083`, NamedDisk ana journal | Sayfa numarası, preimage ve checksum ayrı xWrite çağrılarıdır. Ofsetler `iOff`, `iOff + 4` ve `iOff + 4 + pageSize`. Her hata sonraki yazımları durdurur. | `lib.rs:6437–6450`. Geçerli dosyalarda dört sayfa boyutu ve üç journal kipiyle C izleri eşit. Her alanda IOERR_WRITE ve FULL ile UPDATE ve COMMIT ayrı sınandı. |
| `pager.c:6092–6098`, kayıt tamamlandı | Journal ofseti ve kayıt sayısı ilerler. Ana journal bitvec'i ve sayfayı kapsayan savepoint bitvec'leri bundan sonra güncellenir. | `lib.rs:4025–4031,4047–4073,6452–6456`. Test başarısız parçadan sonra ofset, sayı ve kapsamın ilerlemediğini, rollback sonrasında dosya içeriğinin korunduğunu denetler. |

Kalıcı regresyon `disk_journal_records_advance_only_after_all_three_writes` sağlıklı commit'i ve üç ayrı yazma hatasını kapsıyor. Önceki dosya imajı her başarısız yazmada aynı kaldı ve rollback sonrası yeni bağlantı eski sayfayı okudu. Sağlıklı commit sonrası aynı bağlantı açma dizisi yeni sayfayı okudu. [Açık farklar](gorulen_farklar_v6b_52.md).

Gerçek worktree sürümünde 12/12 sağlıklı native vaka ve 144/144 yazma hatası her iki Rust kipinde C ile eşleşti. Hata matrisinde UPDATE ve COMMIT dönüşleri, sonraki SELECT, integrity_check, hatanın geldiği aşamadaki xWrite izleri, journal varlığı ve uzunluğu ile bütün ana DB özetleri eşit. Ortak nonce ile ikinci koşuda bütün journal görüntüleri de bayt düzeyinde eşit. Ayrı scratch adayındaki 96 sağlıklı COMMIT, D154'ün 36 ve 18, D155'in 60 terminal regresyonu eşit.

Ham sağlıklı matris ve gcov `target/perf-work/v6b56-proof/final-worktree/res`, iki hata matrisi `target/perf-work/v6b56-proof/write-failures/res` altında.

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1149 geçti, 7 başarısız, 337 yok sayıldı. ON 1135 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.51 ile aynı. Yeni pager regresyonu iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.51 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.51 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 305,39 ms, aday 380,38 ms, süre oranı 1,246.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/v6b56_verify/res` altında.
