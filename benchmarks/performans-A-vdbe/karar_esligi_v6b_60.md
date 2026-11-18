# V6b.60 — D164 V1 iç PRAGMA hata ofseti

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_60_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pragma.c:3010–3025`, otomatik `pragma_locking_mode` iç ifadesi | İç PRAGMA SQL'i şema token'ı ile hazırlanır. Başarısız iç hazırlama şema tanısını taşır. | `sqliters-core/src/vtab.rs:912–924,3836–3846` ölçülen otomatik modülü ve üretilmiş iç SQL'i tanır. |
| `build.c:1053–1054`, bilinmeyen şema | İç SQL'deki şema token'ının başlangıcı bayt ofseti 7 olur. | `vtab.rs:921` oluşturulan PRAGMA önekinin bayt uzunluğunu step sonucu için saklar. |
| `vdbeapi.c:951–956`, `vdbeaux.c:3565`, dış API | V1 step iç ofseti korur. Saved SQL ile V2 hata aktarımı ofseti temizler. | `sqliters-core/src/vdbe_host.rs:1584–1602,1633` per-step bilgiyi taşır. `sqliters-core/src/lib.rs:7658–7671` V1 step sonucuna ofseti verir ve kapanış hatasını ofsetsiz saklar. |

Yeni kalıcı test V1 step/reset/step/finalize ofsetlerini `7,-1,7,-1`, V2 ofsetlerini `-1,-1,-1,-1` ve doğrudan PRAGMA hazırlama tanısını iki kipte 7 olarak denetliyor. [Açık farklar](gorulen_farklar_v6b_60.md).

Gerçek worktree 184/184 geniş native işlemde C ile eşit. Önceki sekiz V1 farkı kapandı, diğer 176 iz ve bütün fiziksel DB özetleri korundu. Odak 8/8 V1 ve 8/8 V2, doğrudan hazırlama ile yaşam döngüsü kontrolleri ve iki kipte kalıcı test geçti. Ham kayıtlar `target/perf-work/v1-locking-proof/final-worktree/res` altında.

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1157 geçti, 7 başarısız, 337 yok sayıldı. ON 1143 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.59 ile aynı. V1 ve önceki D52 ile D163 regresyonları iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.59 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.59 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 301,79 ms, aday 372,74 ms, süre oranı 1,235.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/v1-offset-verify/res` altında.
