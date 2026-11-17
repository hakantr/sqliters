# V6b.37 — İlk şema sayfası ve V1 yürütme

Boş veritabanında yazma işlemi ilk şema sayfasını ayırır. Başlangıç çerezi ve şema biçimi değişmediği sürece sayfa tahsisi hazırlanmış V1 ifadesini geçersiz kılmaz.

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [Dal kapsamı ve alınmayan çıkışlar](karar_esligi_v6b_37_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kapsam |
|---|---|---|
| `btree.c:3540–3576,3743–3754`, B-tree boş ve yazma işlemi başlıyor | `newDatabase` sayfa 1'i ayırıp başlık alanlarını sıfırlar. Var olan sayfalı veritabanında bu işlem atlanır. | `Pager::begin_write_internal` ve `initialize_empty_database` önceki tahsis yolunu kullanır. `has_uncommitted_empty_schema_page` işlem öncesi sıfır sayfayı, güncel tek sayfayı, şema biçimi 0'ı ve değişmemiş çerezi birlikte denetler. |
| `btree.c:3817–3845`, işlem başladı | Şema çerezi sayfa 1'in 40. baytındaki değerden okunur. | Yeni pager koşulunda önceden yüklenmiş boş şema aynı kalır. `ConnectionInner::refresh_open_tx_schemas` ilk sayfa tahsisi için şema nesnesini yeniden yükleyip nesil sayaçlarını artırmaz. |
| `vdbe.c:4318–4354`, `OP_Transaction` şema denetimi | `P5` etkinse yalnız çerez veya şema nesli değiştiğinde `SQLITE_SCHEMA` oluşur. | Sayfa oluşumundan kaynaklanan yalancı eskime kaldırıldı. Gerçek çerez değişikliği, şema nesli ve ayrılan şema denetimleri sürer. Mevcut tablo oluşturma ve `PRAGMA schema_version` ile 48 eskime kontrolü yapıldı. V1'in C API hata maskelemesi açık. |

468 native akışta 402 eşleşme var. Önceki adımda bu kümede 330 eşleşme vardı. Önceden eşleşen akışların tamamı korundu. V6b.36 üretilen sütun matrisindeki 10 eski V1 akışı kapandı, matris 244/244 oldu. 48 gerçek şema değişikliği akışının ham karşılaştırması V6b.36 ile aynı. [Açık farklar](gorulen_farklar_v6b_37.md).

Ham kanıt dizini `/home/hakan/github/sqliters/target/perf-work/v6b39/`. `fresh_schema.py`, `expire.py`, `fresh_coverage.py`, `check.sh`, `gates.sh` ve önceki dilim matrisleri ölçümleri üretir. `res/source_manifest.json`, `res/oracle_and_binaries.json`, `res/validation.txt` ve `res/verified_commit.txt` gözlemleri sürüme bağlar.

| Yayın kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1138 geçti, 4 başarısız, 337 yok sayıldı. ON 1124 geçti, 18 başarısız, 337 yok sayıldı. Başarısız test kümeleri önceki sürümle aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları önceki sürümle aynı. |
| Ayrışma bulucu | 245 senaryonun stdout'u ve 244 senaryonun stderr'i C ile eşit. Önceki özet bayt düzeyinde aynı. |
| Rust 1.100 tabanına hız, yedi deneme | Toplam medyan taban 299,23 ms, aday 367,51 ms. Aday/taban 1,228. 14 digest eşit. |

Önceki 928 DDL, 250 mmap, 52 büyüme, 270 TEMP, 144 tam program ve 12 etkin snapshot kaydı ham olarak aynı. Genişletilmiş mmap 433/440 kaldı ve yedi eski farkın ham kayıtları aynı.
