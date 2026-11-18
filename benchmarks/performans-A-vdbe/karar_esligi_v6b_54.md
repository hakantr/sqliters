# V6b.54 — D158 journal bölüm başlığında kısa okuma

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_54_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pager.c:4388–4394`, sonraki hizalı sekiz bayt | Okuma dolu ve magic geçerliyse bir bayt geçersizleştirilir. Tam okuma ve farklı magic varsa journal'a yazılmaz. | `sqliters-pager/src/lib.rs:2096–2106` yalnız tam okuma ve geçerli magic için bir bayt yazar. |
| `pager.c:4394–4396`, kısa okuma | Bu taramada `SQLITE_IOERR_SHORT_READ` kabul edilir ve journal sync kararı sürer. | `lib.rs:2102–2110` kısa bayt sayısını ve açık `SQLITE_IOERR_SHORT_READ` dönüşünü kabul eder. Aynı karar taramanın koşullu yazım sonucuna uygulanır. |
| `pager.c:4394–4420`, farklı hata | İlk gerçek tarama hatası nRec yazımı veya sync başlamadan döner. | `lib.rs:2107–2119` diğer hata kodlarını olduğu gibi döndürür. |

Yeni kalıcı test olağan kısa okuma ile açık `SQLITE_IOERR_SHORT_READ` sonrasında aynı başlık ve sync sırası, DB görüntüsü ve yeniden okuma sonucu arıyor. `SQLITE_IOERR_READ` halinde eski veri korunuyor. D157 ve D156 odak testleri iki Rust kipinde de yeniden geçti.

Gerçek worktree'de doğal EOF için 16/16 vaka her iki Rust okuma gösteriminde C ile eşit. Önceki geçerli PERSIST journal ve gerçek hata dönüşlerini kapsayan ayrı 14/14 vaka da eşit. Önceki başarılı PERSIST işleminin bıraktığı farklı magic için NORMAL, FULL ve EXTRA'da 3/3 eşit. Hedef VFS sırası, COMMIT sonucu, sonraki okuma, bütünlük denetimi ve ana DB özetleri karşılaştırıldı. Journal varlığı ve uzunluğu ile sonlandırılan başlık baytları kaydedildi. Ham PERSIST özetleri bağımsız nonce içeriyor.

[Açık farklar](gorulen_farklar_v6b_54.md).

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1151 geçti, 7 başarısız, 337 yok sayıldı. ON 1137 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.53 ile aynı. D158 ve önceki iki pager regresyonu iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.53 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.53 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 298,29 ms, aday 373,63 ms, süre oranı 1,253.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/v6b58_verify/res`, native karşılaştırma `target/perf-work/v6b58-proof/final-worktree/res` altında.
