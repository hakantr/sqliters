# V6b.64 — F9 REGEXP hazırlama tanıları

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Başlangıç V6b.63 `717f673db1996d75b2741ce845aa286d8db6135d`.

| C kararı | C sonucu | Rust karşılığı |
|---|---|---|
| `parse.y:1366–1389`, `expr.c:1203` | Infix operatör tokenı, fonksiyon adını ve SQL içindeki UTF-8 bayt ofsetini koruyor. Argümanlar pattern, değer ve varsa ESCAPE sırasıyla geliyor. | Var olan C hazırlama portu operatör ve doğrudan çağrı tokenlarını çözüyor. Rust AST ve yaşam döngüsü API'si korunuyor. |
| `resolve.c:1166–1178, 1308–1326`, isim yok | Hazırlama kodu 1, `no such function: ReGeXp`, özgün token ofseti. | `check_c_prepare_error` mevcut SELECT hata çıktısından bu tanıyı taşıyor. |
| Aynı karar, isim var ve arite yok | Hazırlama kodu 1, `wrong number of arguments to function ReGeXp()`, aynı ofset. | Aynı yol bu tanıyı taşıyor. Başarılı 2 ve 3 argüman kaydı önceki yürütme yolunda kalıyor. |

54 native vaka V1/V2 ve UTF-8, UTF-16le, UTF-16be veritabanlarını kapsıyor. OFF 12/54'ten, ON 26/54'ten 54/54'e çıktı. Hazırlamada 42 hata ve 12 LIKE/GLOB kontrolü eşit. 72 kayıtlı fonksiyon vakasında 30 hazırlama hatası, 42 başarılı çağrı, callback argüman sırası, sonuç, step/reset/finalize ve dosya SHA-256'ları C ile iki kipte de eşit. 6 stored-view vakası C ile eşit. Group54'ün iki mesaj beklentisi C ile ölçülen yazım ve ofsetlere taşındı. Bütün callback test dosyası iki kipte 12/12 geçti.

Rust hazırlama API'si SQL'i `&str` alıyor. Native C tarafında `sqlite3_prepare16` ve `sqlite3_prepare16_v2` için de C'nin UTF-8'e çevrilmiş bayt ofseti 24/24 ayrı girdi ölçümüyle doğrulandı. [C dal kapsamı](karar_esligi_v6b_64_dallar.md) · [Açık farklar](gorulen_farklar_v6b_64.md).

| Kapı | Sonuç |
|---|---|
| Çalışma alanı | OFF 1161 geçen, 7 başarısız, 337 yok sayılan. ON 1148 geçen, 20 başarısız, 337 yok sayılan. Group54 C beklentisiyle iki kipte geçiyor. Yeni başarısızlık yok. |
| DML fuzzer | OFF 380/400, ON 400/400. Ham özetler V6b.63 ile aynı. |
| Ayrışma bulucu | 245/245 stdout, 244/245 stderr. Ham özet V6b.63 ile aynı. |
| Hız ve sonuç digest'i | 298,63 ms taban, 367,97 ms aday, 1,232 kat. 14/14 digest eşit. |

147/147 kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküşü karşılaştırması korundu. Dört kapının ham kanıtı `/home/hakan/github/sqliters/target/perf-work/regexp-prepare-verify/res` içinde.

Ham kanıtlar `/home/hakan/github/sqliters/target/perf-work/regexp-prepare-proof` ve `/home/hakan/github/sqliters/target/perf-work/regexp-contract-audit` altında. Adaya aktarılan iki dosyanın bayt özetleri `res/candidate-sha256.txt` ile doğrulandı.
