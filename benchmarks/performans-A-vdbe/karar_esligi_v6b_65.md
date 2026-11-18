# V6b.65 — REGEXP callback değeri ve bellek PRAGMA beklentisi

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Başlangıç V6b.64 `afc3019be8c8263f0dc1566791e92f96cff573f2`. İki ayrı native doğrulama alanı tek dört kapı koşusuna alındı.

| C kararı | C sonucu | Rust karşılığı |
|---|---|---|
| `parse.y:1366–1389`, `vdbe.c:9027–9078` | Pozitif infix REGEXP, fonksiyonun döndürdüğü değeri ve türünü kullanıyor. İki aritede desen, değer ve varsa ESCAPE sırasıyla veriliyor. | Legacy compiler, pozitif REGEXP çağrısının sonucunu doğrudan hedef kayıtçıya yazıyor. Legacy interpreter aynı `RuntimeValue` değerini döndürüyor. |
| `parse.y:1374–1388`, `vdbe.c:2726–2735, 2819–2825` | `NOT REGEXP` ayrı NOT işlemi, WHERE ayrı koşul değerlendirmesi. NOT sonucu NULL girdide NULL. | Mevcut NOT ve filtre yolları korunuyor. Sayısal BLOB dönüşümü ayrı açık kapsam. |
| `pragma.c:526, 951–977` | Bellek DB'de `PRAGMA mmap_size` kolon adını bildiriyor. `SQLITE_FCNTL_MMAP_SIZE` NOTFOUND ile bitiyor ve sonuç satırı üretilmiyor. Diskte destekli file control güncel sınırı döndürüyor. | Mevcut `prepare_row_pragma` davranışı C ile eşit. Üç eski test beklentisi sıfır satır olacak şekilde düzeltildi, üretim PRAGMA kodunda değişiklik yok. |
| `pragma.c:2329–2366`, `btree.c:10500` | `data_version` okuma ve assignment sözdizimi bu test kurulumunda aynı metadata ve tek `1` satırı veriyor. | Aynı C karşılaştırmasıyla mevcut üç test beklentisi korundu. |

REGEXP yürütme karşılaştırması üç DB kodlamasında V1/V2, sıradan tablo yolu ve MATERIALIZED CTE, pozitif projeksiyon, NOT ve WHERE, iki arite ve sekiz callback değeri içeriyor. Pozitif 192/192 vaka iki kipte eşit. Tam 576 vaka OFF 416'dan 560'a, ON 480'den 576'ya çıktı. Bütün dosya özetleri eşit. İki kipte kalan 384 NOT/WHERE çıktısı önceki ilgili Rust kipinin ham çıktısıyla birebir. Hazırlama regresyonları 54/54 ve 72/72, phase6 iki kipte 13/13.

Bellek, disk, önceden hazırlanmış disk sonucu ve bağlı DB senaryoları V1/V2 ile iki kipte 8/8 eşit. Sorgu, assignment ve `main.mmap_size` bellekte `mmap_size` adlı bir kolon ve sıfır satır veriyor. Diskte limit 0 → 1048576 → 0, önce hazırlanmış ifade kendi değerini koruyor. `data_version` beklentileri ve dosya özetleri ölçüldü. Odak testi her kipte 1/1 geçti.

| Kapı | Sonuç |
|---|---|
| Çalışma alanı | OFF 1163 geçen, 6 başarısız, 337 yok sayılan. ON 1150 geçen, 19 başarısız, 337 yok sayılan. Eski PRAGMA beklentisi iki kipte düzeldi. Yeni başarısızlık yok. |
| DML fuzzer | OFF 380/400, ON 400/400. Ham özet V6b.64 ile aynı. |
| Ayrışma bulucu | 245/245 stdout, 244/245 stderr. Ham özet V6b.64 ile aynı. |
| Hız ve sonuç digest'i | 301,72 ms taban, 370,29 ms aday, 1,227 kat. 14/14 digest eşit. |

147/147 kurtarma, 12/12 dosya imajı ve 4/4 gerçek C çöküşü kontrolü eşit. Dört kapının ham ölçümü `/home/hakan/github/sqliters/target/perf-work/regexp-value-verify/res` içinde.

Ham ölçümler `/home/hakan/github/sqliters/target/perf-work/regexp-prepare-proof/value-parity/res` ve `/home/hakan/github/sqliters/target/perf-work/mmap-data-version-audit/res` altında. [Dal kapsamı](karar_esligi_v6b_65_dallar.md) · [Açık farklar](gorulen_farklar_v6b_65.md).
