# V6b.53 — D157 rollback journal cihaz yetenekleri

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_53_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pager.c:1531–1539`, yeni journal başlığı | Sync OFF, MEMORY veya ana DB VFS'i SAFE_APPEND bildirdiğinde magic ile `0xffffffff` birlikte yazılır. Diğer sync açık disk yollarında ilk 12 bayt sıfırdır. | `sqliters-pager/src/lib.rs:2146–2169` ve `sqliters-vfs/src/lib.rs:47–50` aynı ana DB tanıtıcısını ve bayrağı kullanıyor. |
| `pager.c:4350–4419`, SAFE_APPEND ve SEQUENTIAL | SAFE_APPEND açıkken sonraki başlık kontrolü ve 12 baytlık magic/nRec yazımı atlanır. SEQUENTIAL kapalı olduğunda FULL/EXTRA, başlığın yenilenmesinden önce journal sync yapar. | `sqliters-pager/src/lib.rs:2070–2112` EXCLUSIVE alarak aynı sırayı uygular. D158, sonraki başlık okumasının dönüş gösteriminde kalan farktır. |
| `pager.c:4421–4434`, sync ve yeni bölüm | SEQUENTIAL fiziksel son sync'i de kaldırır. Spill yeni bölüm istiyorsa yalnız SAFE_APPEND kapalıyken sayaç sıfırlanır ve başlık eklenir. | `lib.rs:2114–2125` aynı koşulları kullanır. PERSIST sonlandırmasının kendi sync işlemi korunur. |
| `pager.c:4439–4444`, başarı sonrası cache | Journal işlemleri tamamlanınca NEED_SYNC temizlenir ve pager DBMOD olur. | `lib.rs:2128–2131` cache geçişini başarı sonrasına bırakır. |

32 vakalık cihaz matrisi dört yetenek birleşimini, dört synchronous düzeyini ve DELETE/PERSIST kiplerini kullanıyor. Dört spill vakası NORMAL ve FULL'da SAFE_APPEND'in ilk bölümü koruduğunu ve güvenli bit kapalıyken aynı yeni bölüm sınırlarının oluştuğunu ölçüyor. Bu yetenekler özel test VFS'inde bildirilir. Olağan Unix VFS mevcut cihaz bildirimiyle çalışır. Kalıcı pager testi dört bit birleşiminde ilk başlığı, journal sync sırasını ve PERSIST sonlandırmasını denetler.

Gerçek worktree'de 32/32 cihaz ve 4/4 spill vakasında VFS sırası, sorgu sonucu, bütünlük denetimi ve ana dosya özetleri eşit. Aynı cihaz matrisi `fullfsync=ON` ile 32/32 eşit. C ve Rust, istenen ön sync için FULL ve son sync için FULL|DATAONLY verdi. D156'nın 144 yazma hatası iki kipte tekrar eşleşti. Ortak nonce ile journal görüntüleri de eşit.

Ham kayıtlar `target/perf-work/v6b57-proof/final-worktree/res`, `target/perf-work/v6b57-proof/fullfsync/res` ve `target/perf-work/v6b56-proof/write-failures-d157/res` altında.

[Açık farklar](gorulen_farklar_v6b_53.md).

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1150 geçti, 7 başarısız, 337 yok sayıldı. ON 1136 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.52 ile aynı. Yeni D157 testi ve D156 regresyonu iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.52 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.52 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 298,19 ms, aday 372,00 ms, süre oranı 1,248.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/v6b57_verify/res` altında.
