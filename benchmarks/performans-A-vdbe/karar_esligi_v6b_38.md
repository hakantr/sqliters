# V6b.38 — D103 pragma_locking_mode gizli sütun filtresi

`pragma_locking_mode('aux')` için birden çok eşitlik koşulu varsa C planlayıcısının tüketmediği koşul çıktı satırında denetlenir. Bu üretim ve yürütme yolu, desteklenen tek kaynaklı sorgularda makine anahtarının iki durumunda kullanılır.

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [Alınan ve alınmayan C dalları](karar_esligi_v6b_38_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kapsam |
|---|---|---|
| `where.c:6948–6985`, WHERE ve tablo işlevi argümanları | Açık WHERE terimleri ayrılır, ardından tablo işlevi argümanı için ek gizli sütun koşulu gelir. | `where_.rs` ve `whereexpr.rs::sqlite3_where_tab_func_args` C sırasını üretir. Tek `pragma_locking_mode` kaynağı için program makine kapalıyken de kullanılabilir. |
| `pragma.c:2896–2931`, aynı gizli sütunda birden çok uygun eşitlik var | Görülen son eşitlik `argvIndex` ve `omit` alır. Kalan koşullar programda yer alır. | `PragmaVTab::best_index_calls` ve makine `VFilter`, `VColumn` ile kalan karşılaştırmayı yürütür. Otomatik kayıt ve desteklenen program denetimleri özel modüllerin ve başka tabloların yolunu korur. |
| `pragma.c:2998–3025`, şema argümanı bir iç PRAGMA üretir | `%Q` şemayı tek tırnakla yazar. Bilinmeyen şema tanısı aynı token'ı içerir. | `PragmaVTabCursor::filter` nitelemeyi aynı biçimde kurar ve iç PRAGMA tanısında korur. V1 iç hata ofseti ayrıca açık. |

184 native akışın 176'sı eşit. Önceki 144 eşleşme korundu. Önceki 488 bağlantı/kilit akışındaki kayıtlı D103 kapandı. Diğer 12 farkın ham çıktıları ve dosya hash'leri öncekiyle aynı. [Açık farklar](gorulen_farklar_v6b_38.md).

Ham kanıt dizini `/home/hakan/github/sqliters/target/perf-work/v6b38/`. `locking_tvf.py`, `old_connection_matrix.py`, `manual_cli.py`, `locking_coverage.py`, `check.sh` ve `gates.sh` ölçümleri üretir. `res/source_manifest.json`, `res/oracle_and_binaries.json`, `res/validation.txt` ve `res/verified_commit.txt` gözlemleri sürüme bağlar.

18 kabuk akışında veri, hata/uyarı çıktısı ve dosya hash'leri eşleşti. Önceki 928 DDL, 468 ilk şema sayfası, 48 eskime, 244 üretilen sütun ve mmap/TEMP matrislerinin ham kayıtları aynı.

| Yayın kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1139 geçti, 4 başarısız, 337 yok sayıldı. ON 1125 geçti, 18 başarısız, 337 yok sayıldı. Başarısız test kümeleri önceki sürümle aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları önceki sürümle aynı. |
| Ayrışma bulucu | 245 senaryonun stdout'u ve 244 senaryonun stderr'i C ile eşit. Önceki özet bayt düzeyinde aynı. |
| Rust 1.100 tabanına hız, yedi deneme | Toplam medyan taban 298,55 ms, aday 366,57 ms. Aday/taban 1,228. 14 digest eşit. |
