# V6b.36 — Üretilen sütun kısıtlarının hazırlama sırası

Üretilen sütun PRIMARY KEY veya DEFAULT ile çakıştığında C parser'ına taşınan kararlar hazırlamada uygulanır. Kısıtların yazılma sırası tanıyı belirler. Var olan tabloya ya da görünüme yönelik `CREATE TABLE IF NOT EXISTS` şemayı okuma işlemiyle doğrular ve sütun gövdesini işlemeden çıkar.

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [Dal kapsamı ve alınmayan çıkışlar](karar_esligi_v6b_36_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kapsam |
|---|---|---|
| `build.c:1866–1877,1909–1934`, PK işaretlenirken kolon üretilmiş | Aynı tanı tek kolon, tablo PK'si ve birleşik PK için hazırlamada oluşur. Önceden kaydedilmiş ayrı bir PK varsa ikinci PK hatası önce gelir. | `build_tab.rs::make_column_part_of_primary_key` ve `sqlite3_add_primary_key` işlemleri `check_create_generated_column_errors` üzerinden gerçek hazırlamada kullanılır. |
| `build.c:2038–2087`, üretilen kolon PK'den sonra bildirilmiş | Her iki depolama türü için PK tanısı üretilir. Öncesinde DEFAULT veya geçersiz depolama türü varsa üretilen sütun tanısı önce gelir. | `build_tab.rs::sqlite3_add_generated` aynı parser sırasını uygular. Yeni ve otomatik yeniden hazırlama aynı denetimden geçer. |
| `build.c:1798–1809`, DEFAULT bir üretilen kolonu izliyor | Önce ifadenin sabitliği denetlenir. İfade uygunsa üretilen sütunda DEFAULT reddedilir. | `build_tab.rs::sqlite3_add_default_value`. Aynı DDL'ye eklenen PK, kolasyon ve bilinmeyen ifade örneklerinde hata önceliği ölçüldü. |
| `build.c:1343–1352,1911,2043–2046`, mevcut tablo/görünüm ve IF NOT EXISTS | Şema doğrulanır ve `pNewTable` kurulmaz. PK veya DEFAULT eylemleri tablo gövdesini değiştirmez. | C parser'ı hazırlamayı kabul eder. `exec_create_table_started` şemayı okuma işlemi altında yeniler ve hedef hâlâ varsa yazar kilidini almadan çıkar. Başka bir yazıcının RESERVED kilidi altında denetlendi. |

244 native akışın 234’ünde çıkış kodu, stdout, stderr ve ana/bağlı dosya SHA-256 değerleri eşit. V6b.35 ile aynı girişlerde 82 eşleşme vardı. Kalan 10 V1 farkı önceki adayla ham olarak aynı. Matris V1/V2, iki makine kipi, ana/TEMP/bağlı şema, ad çakışması, geçerli PK/UNIQUE ve çakışan kısıt sırasını kapsar. Authorizer etkin bağlantılarda mevcut hazırlama yolu kullanılır.

Ham kanıt dizini `/home/hakan/github/sqliters/target/perf-work/v6b37/`. `generated_pk.py`, `manual_cases.py`, `coverage37.py`, `check_final.sh` ve `gates_final.sh` ölçümleri üretir. `res/source_manifest.json`, `res/oracle_and_binaries.json`, `res/validation.txt` ve `res/verified_commit.txt` gözlemleri sürüme bağlar. [Açık farklar](gorulen_farklar_v6b_36.md).

| Yayın kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1137 geçti, 4 başarısız, 337 yok sayıldı. ON 1123 geçti, 18 başarısız, 337 yok sayıldı. Başarısız test kümeleri önceki sürümle aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. İki kipte 6041 ve 6212 kapandı. Kalan farkların ham kayıtları aynı. |
| Ayrışma bulucu | 245 senaryonun stdout'u ve 244 senaryonun stderr'i C ile eşit. Önceki özet bayt düzeyinde aynı. |
| Rust 1.100 tabanına hız, yedi deneme | Toplam medyan taban 297,32 ms, aday 367,88 ms. Aday/taban 1,237. 14 digest eşit. |

22 kabuk akışı aynı. Önceki 928 DDL, 250 mmap, 52 büyüme, 270 TEMP, 144 tam program ve 12 etkin snapshot kaydı ham olarak aynı. Genişletilmiş mmap 433/440 kaldı ve yedi eski farkın ham kayıtları aynı.
