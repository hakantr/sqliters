# V6b.67 — SQL koşullarında sayısal BLOB

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Başlangıç V6b.66 `f7f1bfda88d753f4c471a7411f28f79af43d82b2`.

| C kararı | C sonucu | Rust karşılığı |
|---|---|---|
| `vdbemem.c:791–796` | INTEGER kendi değeriyle, NULL çağıranın null kuralıyla, diğer türler sayısal değerle değerlendirilir. | Legacy SQL, mevcut değer davranışını koruyup BLOB için kodlamalı sayısal dönüşümü çağırıyor. |
| `vdbemem.c:674–718, 733–750, 770–782` | Metin ve BLOB sayısal öneki hücre kodlamasıyla okunur. UTF-8 ve iki UTF-16 byte sırası farklı BLOB'ları sayı olarak alabilir. | `sqlite_atof_with_encoding` mevcut SQLite ayrıştırıcısını ve UTF-16 önek hesabını paylaştırıyor. Legacy VDBE, interpreter, hızlı okuma ve FILTER yolları yürütme bağlamındaki kodlamayı veriyor. |
| `vdbe.c:2726–2735, 2819–2825` | NOT NULL için NULL üretir. WHERE koşulu programın null kararını kullanır. | Beş legacy mantık ve dal opcode'u, CASE, filtre ve ilgili yazma koşulları aynı kodlamalı boolean değeri alıyor. |

UTF-8, UTF-16le ve UTF-16be için literal, bind, tablo sütunu ve MATERIALIZED CTE üzerinden 2160 `NOT`, `CASE`, `AND/OR`, `WHERE` ve sabit kontrolü ölçüldü. OFF 1852/2160 ve ON 2020/2160 tabanından iki kipte 2160/2160'a çıkıldı. Dosya özetleri eşit. UTF-8 için `x'32'`, UTF-16le için `x'3200'`, UTF-16be için `x'0032'` karşılaştırmalarının her birinde C ve Rust kendi veritabanı kodlamasında aynı sonucu verdi.

Önceki REGEXP tip matrisi 576/576 iki kipte aynı. 12 UPDATE/DELETE, partial index ve trigger WHEN vakası, 6 aggregate FILTER/HAVING ve window FILTER vakası ile 90 normal fonksiyon sonucu aynı. Kalıcı odak testi her kipte 1/1, phase6 her kipte 13/13 geçti. Types 18/18 ve VDBE 12/12.

`Value::truthiness()` dış API varsayılanı korunuyor. Ayrı `IS TRUE/FALSE` gramer kontrolleri ve zorlanan UTF-16 C VDBE BLOB dönüş değeri farkları ham kayıtta duruyor. [C dal kapsamı](karar_esligi_v6b_67_dallar.md) · [Açık farklar](gorulen_farklar_v6b_67.md).

| Kapı | Sonuç |
|---|---|
| Çalışma alanı | OFF 1166 geçen, 6 başarısız, 337 yok sayılan. ON 1153 geçen, 19 başarısız, 337 yok sayılan. Yeni doğruluk testi iki kipte geçti. Başarısız kümeler V6b.66 ile aynı. |
| DML fuzzer | OFF 380/400, ON 400/400. Ham özet V6b.66 ile aynı. |
| Ayrışma bulucu | 245/245 stdout, 244/245 stderr. Ham özet V6b.66 ile aynı. |
| Hız ve sonuç digest'i | 298,43 ms taban, 373,80 ms aday, 1,253 kat. 14/14 digest eşit. |

147/147 kurtarma, 12/12 dosya imajı ve 4/4 gerçek C çöküşü kontrolü eşit. Kaynak ve ham ölçümler `/home/hakan/github/sqliters/target/perf-work/numeric-blob-boolean-proof/candidate` ve `/home/hakan/github/sqliters/target/perf-work/numeric-blob-verify/res` içinde.
