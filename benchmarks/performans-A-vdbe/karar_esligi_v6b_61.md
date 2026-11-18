# V6b.61 — D165 prepared UPDATE ve DELETE RETURNING sayaçları

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_61_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `vdbe.c:1807–1836`, UPDATE/DELETE RETURNING satırı | İlk satır `SQLITE_ROW` ile döner. Önceki üst değişiklik sayacı yürürlükte kalır ve çalışmış trigger katkıları toplam sayaçta görünür. | `sqliters-core/src/lib.rs:11080–11086` prepared UPDATE ve DELETE için mevcut D52 geçişini seçer. `sqliters-core/src/engine.rs:26014–26022` üst sayacı başarılı kapanış için saklar. |
| `vdbeaux.c:3493–3502`, son adım | Başarılı Halt üst değişiklik sayısını bağlantıya tek seferde aktarır. | `lib.rs:2252–2447,11549–11558` mevcut statement kapanış yöntemini kullanır. İlk ve son satır, DONE, reset ve finalize ölçülür. |
| `vdbeaux.c:3493–3502`, INSERT ve OR FAIL kontrolleri | INSERT için ölçülen aynı sayaç sözleşmesi ve kısıt sonucu korunur. | D52 INSERT 27 ve INSERT OR FAIL 33 vaka ayrı korumalar olarak karşılaştırılır. |

Kalıcı test UTF-16le ile legacy UPDATE ve DELETE, trigger katkısı, ilk RETURNING satırı ve kapanış sayaçlarını denetliyor. V1 ve D52 odak testleri iki Rust kipinde korundu. [Açık farklar](gorulen_farklar_v6b_61.md).

Gerçek worktree'de iki Rust kipinde 36/36 UPDATE/DELETE, 27/27 INSERT ve 33/33 INSERT OR FAIL C ile eşit. Ana DB özetleri, sorgu ve API sayaçları, bütünlük ve ölçülen yeniden açılışlar eşleşti. Eski geniş UPDATE/DELETE koşusu OFF 0/36 ve ON 12/36 idi. Eski fark ilk RETURNING satırında üst DML sayacının erken görünmesiydi. Ham native kayıtlar `target/perf-work/d165-proof/final-worktree/res` altında.

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1158 geçti, 7 başarısız, 337 yok sayıldı. ON 1144 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.60 ile aynı. Yeni D165 ve önceki V1/D52 regresyonları iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.60 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.60 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 298,46 ms, aday 370,14 ms, süre oranı 1,240.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/d165-verify/res` altında.
