# V6b.57 — D52 prepared INSERT RETURNING sayaçları

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_57_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `vdbe.c:1807–1836`, RETURNING satırı | SQLITE_ROW döner. Üst DML değişiklik sayacı ifade kapanışına kadar bekler. Tamamlanmış trigger etkileri ve last_insert_rowid bağlantıda görülebilir. | `sqliters-core/src/engine.rs:26012–26020` ve `sqliters-core/src/lib.rs:11069–11071,11132–11138` prepared legacy INSERT'in bekleyen sayısını satırlarla taşır. |
| `vdbeaux.c:3493–3502`, ifade kapanışı | Başarılı Halt sonrasında bağlantı değişiklik sayacı bir kez güncellenir. Reset ve finalize da ifade kapanışını tamamlayabilir. | `lib.rs:2252–2447,2996–3000,11546–11554` DONE ve kapanış yollarının sayıyı başarılı ifade tamamlanışında yayımlamasını sağlar. |
| `vdbeaux.c:3476–3502`, commit reddi veya geri alma | Üst INSERT sayısı beklemede kalır. Geri alma mevcut işlem sonucu ve sayaç kurallarını izler. | `lib.rs:2361–2438` commit sırasında sayaç aktarımını başarıya bırakır. Commit hook ve ertelenmiş foreign key sonuçları ayrı karşılaştırılır. |

Yeni kalıcı test ilk ve son RETURNING satırı, araya giren DML, DONE, reset, finalize ve Drop sonrasında aynı SQL ile sayaçları ve satırları denetler. Mevcut UTF-8 C-makine testi iki Rust kipinde korundu.

Gerçek worktree 27/27 bağımsız yaşam döngüsünde ve 72/72 genişletilmiş vakada C ile eşit. Genişletilmiş matris üç kodlama, birden fazla açık prepared yazıcı, trigger, FK ve boş sonucu kullanıyor. Önceki `defer_foreign_keys` aktif yazıcı matrisinde 24/24, kabul eden ve reddeden commit hook vakalarında 2/2 eşit. SQL ve API sayaçları, dönüşler, sonraki tablo sonuçları ve fiziksel DB özetleri kaydedildi. Altı eski tekrarın beşinde sayaç erken yayımlanmıştı.

Ham kayıtlar `target/perf-work/d52-proof/lifecycle/final-worktree/res` ve `target/perf-work/d52-worktree-extra/res` altında.

[Açık farklar](gorulen_farklar_v6b_57.md).

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1154 geçti, 7 başarısız, 337 yok sayıldı. ON 1140 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.56 ile aynı. Legacy ve mevcut C-makine RETURNING testleri iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.56 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.56 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 302,67 ms, aday 375,31 ms, süre oranı 1,240.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/d52-verify/res` altında.
