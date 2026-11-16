# V6b.1 — PRAGMA üreticisinin ilk diliminde karar eşliği

**Durum: V6b tamamlanmadı.** Bu kayıt doğrulanan ilk dilimi ayırır; kalan işleyiciler ve temsil edilmeyen bağlantı geçmişi açıkça `Unsupported` döner. İnceleme `ileriPlanlama/` dosyalarını girdi olarak kullanmadı.

Başlangıç: `e9ba0e2`, `performans-A-vdbe`. Rust **1.100.0**; C **SQLite 3.54.1**, `$S/bin/sqlite3`; `$S=/home/hakan/github/sqliters/target/perf-work`. Kaynak/ikili/derlem ölçümleri `$S/v6b/` altında. C kaynağı değiştirilmedi; kapsam sürücüsü ve `sqlite3.c` kopyası `target` altında derlendi.

## Kapsam ve kanıt katmanları

Beş gramer kuralı, başvuru pragma kayıt tablosu, bayraklar ve analysis_limit, başlık değerleri, default_cache_size programı, page_size sorgusu, page_count/max_page_count, journal_mode, incremental_vacuum, tablo/indeks/FK bilgileri, foreign_key_check, pragma_list, wal_checkpoint ve geçmiş gerektirmeyen optimize yolları. PRAGMA yürütmesi genel olarak eski yürütücüde kalır; bayrak/analysis_limit için hazırlama zamanı ve dondurulmuş sonuç köprüsü C’ye hizalandı. Bu, bütün PRAGMA yürütme yollarının eşliği iddiası değildir.

| Katman | Sonuç | Yerel kanıt |
|---|---|---|
| EXPLAIN (programlar ve hazırlama hataları) | `statements 3992 \| generator accepted 3695: identical 3695, DIFFERENT 0 \| unsupported (fallback) 297 (of which same as C anyway 0) \| not reached (old parser/other) 0 (same 0)` | `res/explain_final.txt`, `res/explain_final_bugs.json` |
| EXPLAIN QUERY PLAN | `statements 3992 \| generator accepted 3695: identical 3695, DIFFERENT 0 \| unsupported (fallback) 297 (of which same as C anyway 279) \| not reached (old parser/other) 0 (same 0)` | `res/eqp_final.txt` |
| Ardışık ayarlar ve elle yazılmış ifadeler | `statements 154 \| generator accepted 153: identical 153, DIFFERENT 0 \| unsupported (fallback) 1 (of which same as C anyway 0) \| not reached (old parser/other) 0 (same 0)` | `res/flow_final.txt` |
| Tam program (C prepare-only Y sürücüsü / Rust üreticisi) | `{"same": 3667, "unsupported": 279}` | `res/programs_final.txt`, `res/programs.jsonl` |
| Testler, kapalı | 1037 geçti / 0 başarısız / 337 yok sayıldı | `res/tests_last_off.txt` |
| Testler, açık | 1022 geçti / 15 bilinen başarısız / 337 yok sayıldı | `res/tests_last_on.txt` |
| DML farksalı, seed6001..6400 | kapalı 378/400, açık 398/400; önceki başarısız seed kümeleriyle aynı | `res/dmlfuzz_last_off.txt`, `_on.txt` |
| Ayrışma bulucu | 245 senaryo; 241 L0, 2 L1, 1 L4, 1 L6; yalnız önceden bilinen g06 stderr farkı | `res/finder_last.txt` |

Tam program katmanı EXPLAIN satırlarına ek olarak nMem, nCursor, nVar, nResColumn, usesStmtJournal, readOnly, btreeMask, expmask ve tam P4 değerlerini karşılaştırır. `Unsupported` bir eşleşme olarak sayılmaz. Prepare-only normal PRAGMA ile EXPLAIN farklı bağlantı etkileri taşıyabildiğinden bu iki katmanın adetleri eşit olmak zorunda değildir.

Yeni beş regresyon testi `tests/v6b_pragma_codegen.rs` içindedir. C karşılaştırmalı test 270 ifadeyi gerçek C ikilisiyle çalıştırır. Diğerleri hazırlama/step/expire sırasını, sonuç dondurmayı, ham writable_schema maskesini, salt okunur program incelemesinin başka bağlantıya sızmamasını, açılmamış temp ve görünüm geçmişinin yaklaşık modellenmemesini sınar. `isolation_before.txt` ve `dbconfig_mask_before.txt` düzeltme öncesi düşen negatif kontrollerdir.

## C kararı → sonuç → Rust karşılığı

Önceden taşınan yardımcıların kararları V1–V6 kayıtlarındadır. Aşağıda bu dilimin koşulları ve bağlantı köprüsü verilir. Her hiç alınmayan gcov dalı ayrıca [dal sınıflandırmasında](karar_esligi_v6b_1_dallar.md) ayrı satırdadır; alınmayan bir dalın eşliği kanıtlanmış sayılmaz.

| C yeri | Koşul | Sonuç / sınır | Rust yeri |
|---|---|---|---|
| `parse.y:1724–1730` | Beş PRAGMA biçimi, isteğe bağlı şema, parantez/eşittir, eksi işareti | Tokenlar C ile aynı konumlarda sqlite3_pragma çağrısına verilir. | `crates/sqliters-codegen/src/parse.rs:1896` |
| `pragma.c:443–447` | sqlite3GetVdbe; RunOnlyOnce; nMem=2 | Init/Expire ile başlayan program ve C ile aynı bellek başlangıcı. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:450–452` | sqlite3TwoPartName; iDb<0 | Bilinmeyen şema C’nin hata metni ve konumuyla reddedilir. | `crates/sqliters-codegen/src/build_tab.rs:90` |
| `pragma.c:457; main.c:4224–4233` | temp açık değil; EXPLAIN geçici Btree açmaz | Dosya denetimi SQLITE_ERROR verir: “SQL logic error”. Bilinmeyen pragma da bundan önce kaçamaz. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:462–472` | minusFlag ve sqlite3NameFromToken | Eksi sayının ham token metnine eklenir; diğer değerler dequote edilir. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:476–504` | Authorizer ve özel VFS cevapları | Yetkilendirici üreticide Unsupported; olağan NOTFOUND ile kapalı-Btree ERROR modellenir. Özel VFS OK/metin/hata kodu taşınmadı. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:510–516` | pragmaLocate sonucu NULL | Bilinmeyen ad için hata/sonuç üretilmeyen C programı. | `crates/sqliters-codegen/src/pragma.rs:52` |
| `pragma.c:519–521` | PragFlg_NeedSchema | Var olan C şema yükleyicisi çağrılır; hata/enjeksiyon kapsamı ayrıca açık. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:524–530` | NoColumns / NoColumns1 ve RHS varlığı | Sonuç sütun adları canonical pragma.h girdisinden; RHS’li bayrak ayarında sıfır sütun. | `crates/sqliters-codegen/src/pragma.rs:59` |
| `pragma.c:1159–1161` | FLAG: zRight==NULL | Maskenin herhangi bir biti açıksa Int64(1), aksi Int64(0), ResultRow. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:1164–1167` | FLAG: autoCommit==0 | Yalnız ForeignKeys maskeden çıkarılır; ayar etkisiz olsa da OP_Expire üretilir. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:1170–1174` | Boolean doğru; WriteSchema ve Defensive | WriteSchema içeren maske Defensive açıkken etkinleştirilmez; diğer maskeler açılır. | `crates/sqliters-codegen/src/pragma.rs:132` |
| `pragma.c:1176–1188` | Boolean yanlış; writable_schema=RESET | Maske temizlenir. RESET şemaları sıfırlar. DeferFK sayaç yolu yazılmış olsa da yaşam döngüsü desteklenene kadar giriş Unsupported. | `crates/sqliters-core/src/lib.rs:5724` |
| `pragma.c:1192–1197; vdbe.c:8386` | Ayar prepare’da, OP_Expire step’te | Bağlantı bayrağı hazırlamada uygulanır; diğer ifadeler yalnız ayar çalıştırılınca geçersizleşir. | `crates/sqliters-core/src/lib.rs:5769` |
| `main.c:1014–1015` | DBCONFIG_WRITABLE_SCHEMA iki bayrağı birlikte değiştirir | WriteSchema ve NoSchemaError mutlak bağlantı değeriyle birlikte kurulur/temizlenir; etkin yazma izni Defensive’den ayrıca hesaplanır. | `crates/sqliters-core/src/cgen.rs:440` |
| `pragma.c:73–102` | getSafetyLevel(omitFull=1), ilk byte rakam mı? | Rakamla başlayan metinde sqlite3Atoi!=0; ON/YES/TRUE, NO/OFF/FALSE; diğerleri varsayılan. Negatif sayı varsayılanı kullanır. | `crates/sqliters-codegen/src/pragma.rs:132` |
| `pragma.c:2733–2745` | analysis_limit: tam sayı, dönüşüm başarılı ve N>=0 | N & 0x7fffffff hazırlamada saklanır; geçersiz/negatif değer önceki ayarı korur; dönüş değeri prepare’da dondurulur. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:2329–2346` | HEADER_VALUE: RHS var ve ReadOnly değil | Transaction(write=1), SetCookie(cookie,Atoi(RHS)), P5=1. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:2347–2350` | schema_version yazımı ve Defensive | SetCookie aynı operandlarla Noop olur; Transaction yine yazma işlemidir. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:2352–2370` | HEADER_VALUE okuma veya ReadOnly RHS | Transaction(read), ReadCookie, ResultRow; RunOnlyOnce Expire, Reusable ile Noop’a döner. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:553–577` | default_cache_size RHS yok/var | C’nin dokuz kodluk okuma dizisi veya abs(Atoi) ile SetCookie(3); hazırlama cache_size etkisi ayrı kaydedilir. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:598–609` | page_size sorgusu/ayarı | Sorgu gerçek pager değerini Int64 ile döndürür; ayar bu dilimde Unsupported. | `crates/sqliters-codegen/src/pragma.rs:123` |
| `pragma.c:663–682` | page_count / max_page_count | Şema doğrulama, yeni yazmaç, Pagecount veya 0..0xfffffffe aralığına kısılmış MaxPgcnt, ResultRow. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:734–759` | journal_mode değeri yok/geçersiz/önek; OFF+Defensive | C’nin ilk eşleşen öneki ve mod sırası; geçersiz veya engellenen OFF sorguya çevrilir. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:760–775` | journal_mode şema seçimi ve azalan DB döngüsü | Sorgu varsayılan main; ayar niteliksizse tüm açık Btree’ler. UsesBtree + JournalMode + tek ResultRow. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:854–867` | incremental_vacuum sınırı eksik/geçersiz/<=0 | INT_MAX; BeginWrite, Integer, IncrVacuum, sıfır sütunlu ResultRow, AddImm/IfPos döngüsü. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:1217–1229` | table_info RHS, tablo bulunması ve görünüm sütunları | İsimli şemalar doğrulanır; LocateTable(NOERR), primary key ve ViewGetColumnNames. | `crates/sqliters-codegen/src/pragma.rs:674` |
| `pragma.c:1230–1243` | NOINSERT ve info/xinfo farkı | table_info hidden/generated alanları atlar; xinfo hidden=1, virtual=2, stored=3 döndürür. | `crates/sqliters-codegen/src/pragma.rs:674` |
| `pragma.c:1244–1266` | PRIMKEY, INTEGER PRIMARY KEY, default ifade | PK ordinal; generated alanda NULL default; diğerinde ham SPAN tokenı. Tür yoksa boş metin. | `crates/sqliters-codegen/src/pragma.rs:674` |
| `pragma.c:1281–1320` | table_list şemaları; nCol==0; dummy SELECT | Şemasız arama ve karma taramasını yeniden başlatma. Başka şemada aynı ad görünümü gizliyorsa geçmiş temsil edilmediğinden açık Unsupported (D82). | `crates/sqliters-codegen/src/pragma.rs:403` |
| `pragma.c:1322–1343` | table_list ad filtresi ve tür ayrımı | C karma sırası; preferred schema adları; view/virtual/shadow/table, ncol, wr, strict. | `crates/sqliters-codegen/src/pragma.rs:403` |
| `pragma.c:1375–1392` | index_info: indeks yoksa WITHOUT ROWID tablosu | Primary key indeksi denenir; normal info anahtar sütunlarını, xinfo bütün indeks sütunlarını alır. | `crates/sqliters-codegen/src/pragma.rs:735` |
| `pragma.c:1393–1414` | index sütunları, negatif cid ve xinfo ekleri | Sıra/cid/ad; negatif cid’de NULL; desc/coll/key C operand sırasıyla. | `crates/sqliters-codegen/src/pragma.rs:735` |
| `pragma.c:1420–1438` | index_list tablo yok/var | Tablo şeması doğrulanır; indeks bağlı liste sırası, unique, c/u/pk, partial. | `crates/sqliters-codegen/src/pragma.rs:776` |
| `pragma.c:1500–1506` | pragma_list | Başvuru pragma.h tablosunun 66 adı aynı sırayla yüklenir. | `crates/sqliters-codegen/src/pragma.rs:40` |
| `pragma.c:1512–1540` | foreign_key_list ordinary table ve pFKey | En yeni kısıttan başlanır; id/seq, child-parent isimleri; OE eylemlerinin beş adı ve NONE. | `crates/sqliters-codegen/src/pragma.rs:799` |
| `pragma.c:1547–1576` | foreign_key_check tablo seçimi ve yazmaçlar | regResult=3, regRow=7; açık tablo, sütun yazmaç üst sınırı ve şema doğrulama. | `crates/sqliters-codegen/src/pragma.rs:467` |
| `pragma.c:1577–1604` | Parent yok/rowid/unique indeks; eşleme hatası | Eksik parent açılmaz; rowid tablo veya KeyInfo ile indeks açılır; FK mismatch C hatası; nTab en büyük imleç sayısına yükseltilir. | `crates/sqliters-codegen/src/pragma.rs:467` |
| `pragma.c:1606–1635` | Child değer NULL, Found veya SeekRowid | NULL ihlal sayılmaz; FK sütunu kadar affinity metni; parent anahtarına göre Found/SeekRowid. | `crates/sqliters-codegen/src/pragma.rs:467` |
| `pragma.c:1637–1654` | FK ihlalini ve sonraki satırı üretme | Rowid veya WITHOUT ROWID için NULL; parent/id yüklenir, dört sütunlu ResultRow, Next. | `crates/sqliters-codegen/src/pragma.rs:467` |
| `pragma.c:2398–2418; sqlite3.h:10265–10269` | wal_checkpoint türü ve şema | PASSIVE=0, FULL=1, RESTART=2, TRUNCATE=3, NOOP=-1; niteliksiz iBt=12. Checkpoint + üç sütunlu sonuç. | `crates/sqliters-codegen/src/pragma.rs:150` |
| `pragma.c:2522–2549` | optimize mask bit2, bit0x10, analysis_limit | Bit2 yoksa ek iş yok; 0x10 ve etkin limitten C’nin 2000 sınırı belirlenir. | `crates/sqliters-codegen/src/pragma.rs:564` |
| `pragma.c:2550–2589` | optimize tablo/indeks/istatistik/kullanım koşulları | TEMP, sistem tabloları ve non-ordinary nesneler atlanır. İstatistik kullanım geçmişi gerekirse açık Unsupported. | `crates/sqliters-codegen/src/pragma.rs:564` |
| `pragma.c:2591–2625` | optimize ikinci tablo, büyüklük ve debug bit0 | İkinci adayda write transaction; Rewind veya IfSizeBetween; debug String8/ResultRow, normal SqlExec(nLimit). | `crates/sqliters-codegen/src/pragma.rs:564` |
| `pragma.c:2628–2643` | optimize nBtree>100 | Limit 100*nLimit/nBtree, alt sınır100; bütün SqlExec P2’leri güncellenir; Expire. | `crates/sqliters-codegen/src/pragma.rs:564` |

Hazırlama etkileri bağlantıya gerçek prepare sırasında uygulanır. `Connection::c_generated_program` sözleşmesi “Nothing else changes” olan salt okunur bir inceleme arayüzüdür; burada hazırlama etkileri bırakılmaz. Diğer bağlantının yeniden hazırlanmasına sızma sınaması bu nedenle zorunludur.

## C kapsamı ve açık dallar

Kapsam iki ayrı C bağlantı başlangıcıyla (DEFENSIVE=0 ve 1), yalnız kabul edilen ifadeler ve şema kurulumları üzerinden ölçüldü. Desteklenmeyen pragmalar örtük kurulum olarak C’de çalıştırılmadı. Tüm sqlite3Pragma gövdesi, henüz taşınmayan işleyiciler dahil paydadadır; oran tamamlanma yüzdesi değildir.

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 466 / 990 (47.1%) | 337 / 706 (47.7%) |
| **total** | 466 / 990 (47.1%) | 337 / 706 (47.7%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 91.7 | 1 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 44.6 | 675 | 45.5 | 164 |

`never_taken_branches 369`. Ayrıntı: [her alınmayan dalın sınıfı ve Rust yeri](karar_esligi_v6b_1_dallar.md). Bellek/GÇ enjeksiyonu, özel VFS ve yetkilendirici başarı yolları kanıtlanmadı.

## Hız kapısı — Rust 1.100 tabanı

CPU6, tmpfs, 7 iç içe A/B denemesi; taban `$S/v4_bin/base_1100`, aday `$S/v6b/bin`. Bu adımda hız ayarı uygulanmadı. W06’nın önceki V6 ikilisine göre doğrulanan gerilemesi ayrı performans açığıdır; [fark kaydına](gorulen_farklar_v6b_1.md) bakın.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.26      8.80     0.951x     9.19     8.74     9.60    0.92x  OK
W02_create_indexes                5.65      5.67     1.004x     5.61     5.64     8.76    0.65x  OK
W03_pk_point_select              29.99     29.75     0.992x    29.44    29.12    23.30    1.28x  OK
W04_secondary_idx_range           9.50      9.51     1.001x     9.41     9.29     7.46    1.27x  OK
W05_full_scan_filter_agg          4.39      4.27     0.972x     4.38     4.27    16.57    0.26x  OK
W06_join_group_order             10.80     11.05     1.023x    10.77    11.02    28.23    0.39x  OK
W07_window_functions             37.02     37.69     1.018x    36.95    37.55    50.21    0.75x  OK
W08_indexed_update_delete        12.04     11.91     0.989x    11.95    11.82     7.96    1.50x  OK
W09_like_text_scan               12.49     12.45     0.997x    12.41    12.38    18.92    0.66x  OK
W10_jsonb_extract_agg            13.73     13.73     1.000x    13.65    13.62    10.93    1.26x  OK
W11_fts5_index_and_bm25          69.85     70.17     1.005x    69.02    69.35    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.61     39.83     0.981x    39.57    39.13    24.63    1.62x  OK
W13_autocommit_wal_txns           4.56      4.59     1.006x     4.44     4.54     4.12    1.12x  OK
W14_cli_sql_script               38.63     38.89     1.007x    38.51    38.60    26.19    1.49x  OK
TOTAL                           298.51    298.31     0.999x   geomean cand/base = 0.996x   digest mismatches = 0
```

## Yeniden çalıştırma

```bash
cd /home/hakan/github/sqliters/.claude/worktrees/vdbe
export PATH=/home/hakan/github/sqliters/target/perf-work/bin:$PATH
cargo test --test v6b_pragma_codegen
cargo test --workspace --no-fail-fast
SQLITERS_VDBE_EXEC=1 cargo test --workspace --no-fail-fast
python3 tools/generate_pragma_table.py /home/hakan/github/sqliters/target/sqlite-ref/amalg/pragma.h /tmp/pragma_table.rs
```

Derlemler: corpus.sql 726; edge.sql 2700; fkedge.sql 511; hand.sql 55; flow.sql 99. `expdiff6.py` (V6), `programs.py`, `covrun_v6b.py`, `mkproof.py`, `gates.sh` ve sonuçlar `$S/v6b/` içindedir. Program sürücüsü `$S/v6/drv/c/rdrv6` ile worktree `target/v6bdrv/release/rdrv6`; tüm derlemeler Rust1.100 sabitinde yapılır. `res/source_manifest.json` son kaynak SHA256 değerlerini tutar.
