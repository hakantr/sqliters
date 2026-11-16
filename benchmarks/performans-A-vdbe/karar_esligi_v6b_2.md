# V6b.2 — cache_size, journal_size_limit ve secure_delete karar eşliği

Başlangıç **84daa83** (V6b.1). Rust **1.100.0**, C **SQLite 3.54.1**. Bu üç aile programı ve hazırlama etkileriyle taşındı. **V6b tamamlanmadı**; taşınmayan işleyiciler ve geçmiş denetimi eklenen bağlamlar açık Unsupported verir; diğer D71 hazırlama eksikleri fark kaydında açıktır. ileriPlanlama/ değerlendirilmedi.

## C kararı ve Rust karşılığı

| C yeri | Koşul | Sonuç | Rust yeri |
|---|---|---|---|
| `prepare.c:822–836; parse.y:1724–1741` | C derlemesi Error; eski ön ayrıştırıcı ifadeyi kabul etmiş olabilir | Sabit sonuç köprüsü C hata iletisi ve byte konumunu prepare/reprepare çağrısına döndürür. Yalnız Unsupported eski yola bırakılır. | `crates/sqliters-core/src/lib.rs:5784` |
| `parse.y:1724–1741` | PRAGMA nmnum/number ham token; ifade tamsayısı değildir | i64 sınırını aşan hex token reddedilmez; negatif i64 taşması ham değer olarak korunur. Normal SQL ifade denetimi değişmez. | `crates/sqliters-syntax/src/parser.rs:2101` |
| `pragma.c:519–523; prepare.c:343–351` | cache_size PragFlg_NeedSchema; cache_size==0 sqlite3InitOne sırasında | Şema hazırlamada yüklenir; sıfır önbellek boyutu yeniden yüklemede çerezden, çerez sıfırsa -2000 ile başlatılır. | `crates/sqliters-core/src/engine.rs:1541` |
| `prepare.c:343–351` | Daha önce ayarlanmış sıfır olmayan önbellek boyutu | Önceki değer korunur; sıfır değer yalnız şema yeniden yüklenirken yeniden başlatılır. | `crates/sqliters-btree/src/lib.rs:862` |
| `pragma.c:882–889` | cache_size RHS yok/var | Sorgu Schema.cache_size değerini Int64 ile dondurur; ayar sqlite3Atoi sonucunu hazırlamada saklar, sonuç üretmez. | `crates/sqliters-codegen/src/pragma.rs:293` |
| `util.c:1310–1373; pragma.c:887` | sqlite3Atoi / sqlite3GetInt32; taşma ve geçersiz metin | Dönüşümde taşma 0; önek/hex ve eksi C dönüştürücüsüyle. i32::MAX değerine sıkıştırma yapılmaz. | `crates/sqliters-codegen/src/util.rs:79` |
| `btree.c:3017–3023` | sqlite3BtreeSetCacheSize | Pager cache boyutu Btree üzerinden ayarlanır; şema önbellek boyutu kaydı da güncellenir. | `crates/sqliters-btree/src/lib.rs:855` |
| `pragma.c:780–788` | journal_size_limit RHS yok | -2 yalnız sorgudur; pager’ın saklanan i64 değeri Int64/ResultRow olur. | `crates/sqliters-codegen/src/pragma.rs:381` |
| `pragma.c:783–785; util.c:1276–1297` | RHS var; sqlite3DecOrHexToI64 dönüş kodu göz ardı edilir; sonuç < -1 | Dönüşümün değeri kullanılır, -1 altı -1 olur. Hatalı hex/taşma/prefix bu yüzden analysis_limit ile aynı biçimde süzülmez. | `crates/sqliters-codegen/src/pragma.rs:381` |
| `pager.c:7549–7554` | iLimit>=-1 | Yeni sınır pager ve varsa WAL’a uygulanır; sorgu sınırı değiştirmez. | `crates/sqliters-pager/src/lib.rs:1187` |
| `pragma.c:628–633` | secure_delete RHS yok/FAST/diğer boolean | Yoksa mevcut bayrak; FAST=2; diğerinde C sqlite3GetBoolean(default0). | `crates/sqliters-codegen/src/pragma.rs:429` |
| `pragma.c:635–641` | Şema niteliği yok ve ayar var | Bütün açık Btree’ler güncellenir; nitelikli ayar yalnız hedefte, sonuç hedef Btree’den gelir. | `crates/sqliters-core/src/lib.rs:5740` |
| `btree.c:3211–3224` | p==NULL; newFlag>=0 | NULL Btree etkisiz; sorgu değiştirmez; bayrak 0/1/2 olarak korunur. | `crates/sqliters-btree/src/lib.rs:466` |
| `pragma.c:443–447,642,788,884` | RunOnlyOnce; Int64/ResultRow sabitleri | Hazırlanmış sonuç değerleri daha sonraki ayardan etkilenmez. Bu üç işleyici bütün ifadeleri geçersizleştiren OP_Expire üretmez. | `crates/sqliters-core/src/lib.rs:5784` |

Rust iç şemasının DDL sonrası yüklenmesi ile gerçek C şema sıfırlaması ayrıdır: yalnız gerçek RESET sıfır önbelleğin yeniden başlatılmasına izin verir. Host bu başlatma kaydını okur; sıradan DDL sıfır değerini korur. Böylece yalnız sorgulamak ile RESET ardından yeniden yüklemek birbirine karışmaz. c_generated_program salt okunur incelemedir; hazırlama etkilerini bağlantıya bırakmaz.

## Karşılaştırmalar

Yerel kök `$P=/home/hakan/github/sqliters/target/perf-work/v6b2`. Kaynak SHA256 değerleri res/source_manifest.json içinde. C kaynak ağacı değiştirilmedi; kapsam kopyası target altında derlendi.

```text
statements 4442 | generator accepted 4178: identical 4178, DIFFERENT 0 | unsupported (fallback) 264 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
   unsupported     241  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported      10  PRAGMA page_size prepare effect (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 4442 | generator accepted 4178: identical 4178, DIFFERENT 0 | unsupported (fallback) 264 (of which same as C anyway 246) | not reached (old parser/other) 0 (same 0)
   unsupported     241  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported      10  PRAGMA page_size prepare effect (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 180 | generator accepted 179: identical 179, DIFFERENT 0 | unsupported (fallback) 1 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
{"same": 4176, "unsupported": 246}
```

Tam program katmanı nMem/nCursor/nVar/nResColumn, işlem ve Btree bayrakları, bütün işlem kodları ve P4 değerlerini de karşılaştırır. Unsupported sayıları eşlik başarısı değildir. C prepare-only ile EXPLAIN geçici Btree açma ve diğer hazırlama etkilerinde farklı olduğu için katman adetleri aynı olmak zorunda değildir.

Önceki 4.091 girdiye pager.sql 450 ve pager_flow.sql 26 ifade eklendi. C tarafında savunmalı kip kapalı ve açık ayrı ölçülür. Yeni kaynakta toplam sekiz PRAGMA regresyon testi vardır; üçü bu dilimde eklendi. C ikilisi kullanan tablo artık 330 ifadeyi karşılaştırır. İ64 sınırları ve büyük hex ham değerleri iki anahtar durumunda C ile yürütülerek karşılaştırıldı (res/journal_int64_runtime.json). Dosya yeniden açma testi C’nin yazdığı default_cache_size=83 çerezini hazırlamada okur ve sonraki ayara rağmen sonucu 83 olarak korur.

Elle yazılmış API hazırlama/adımlama dizileri C ile aynı; secure_delete=ON yalnız hazırlanıp ardından satır silinen dosyanın baytları ve SHA256 değeri de aynı (res/manual_api.json). Bu, her PRAGMA ve bütün dosya biçimlerinin eşliği iddiası değildir.

## C kapsamı ve hiç alınmayan dallar

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 495 / 990 (50.0%) | 356 / 706 (50.4%) |
| btree.c | 15 / 15 (100.0%) | 4 / 4 (100.0%) |
| pager.c | 5 / 5 (100.0%) | 2 / 2 (100.0%) |
| **total** | 515 / 1010 (51.0%) | 362 / 712 (50.8%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 91.7 | 1 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 47.7 | 675 | 48.3 | 157 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |

prepare.c:343–351 önbellek başlatma bloğu ayrıca raw gcov üzerinde ölçüldü: sıfır/sıfır olmayan mevcut cache_size ve sıfır/sıfır olmayan dosya çerezi yönleri ayrı çalıştırıldı (res/cache_init_coverage.txt). Bu blok ana toplamın dışındadır. Kapsama sqlite3BtreeSetCacheSize, sqlite3BtreeSecureDelete ve sqlite3PagerJournalSizeLimit de eklendi. Üç yardımcıda satır ve dal kapsamı %100; mutex/işletim sistemi/bellek hata sözleşmelerinin tamamı bundan çıkarılamaz. pragma.c paydasında taşınmayan bütün işleyiciler vardır. [Her hiç alınmayan dalın ayrı sınıfı](karar_esligi_v6b_2_dallar.md); bellek/GÇ enjeksiyonu, özel VFS ve yetkilendirici yolları açık.

## Dört kapı

Testler kapalı: (1040, 0, 337); açık: (1025, 15, 337) (geçti/başarısız/yok sayıldı). Açık kipteki 15 başarısız testin adları V6b.1 ile aynı; beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; bütün başarısız seed/sınıf kümeleri V6b.1 ile aynı. Ayrışma bulucu 245 senaryoda 241 L0, 2 L1, 1 L4, 1 L6; bilinen g06 stderr farkı sürüyor.

CPU6, tmpfs, Rust1.100 tabanına karşı 7 A/B denemesi. Hız ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.23      8.78     0.951x     9.16     8.74     9.60    0.92x  OK
W02_create_indexes                5.67      5.68     1.002x     5.64     5.63     8.76    0.65x  OK
W03_pk_point_select              29.89     29.62     0.991x    29.39    29.24    23.30    1.27x  OK
W04_secondary_idx_range           9.55      9.65     1.011x     9.33     9.51     7.46    1.29x  OK
W05_full_scan_filter_agg          4.40      4.24     0.962x     4.40     4.22    16.57    0.26x  OK
W06_join_group_order             10.80     11.01     1.020x    10.77    10.99    28.23    0.39x  OK
W07_window_functions             37.14     37.93     1.021x    36.99    37.70    50.21    0.76x  OK
W08_indexed_update_delete        12.05     11.85     0.983x    11.96    11.74     7.96    1.49x  OK
W09_like_text_scan               12.47     12.31     0.987x    12.42    12.29    18.92    0.65x  OK
W10_jsonb_extract_agg            13.74     13.53     0.984x    13.69    13.45    10.93    1.24x  OK
W11_fts5_index_and_bm25          69.83     69.88     1.001x    68.55    69.68    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.57     39.84     0.982x    39.64    39.51    24.63    1.62x  OK
W13_autocommit_wal_txns           4.71      4.57     0.970x     4.38     4.49     4.12    1.11x  OK
W14_cli_sql_script               38.64     38.93     1.008x    38.38    38.67    26.19    1.49x  OK
TOTAL                           298.70    297.82     0.997x   geomean cand/base = 0.991x   digest mismatches = 0
```

W06 için ayrıca önceki V6b.1 ikilisine karşı 15 deneme:

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W06_join_group_order             11.08     11.02     0.994x    11.04    10.98    28.23    0.39x  OK
TOTAL                            11.08     11.02     0.994x   geomean cand/base = 0.994x   digest mismatches = 0
```

C derleme hataları ve CLI caret/byte konumları iki kipte ayrıca karşılaştırıldı (res/pragma_error_after.json). Farklar, düzeltme öncesi kanıt ve kalan iş sırası [gorulen_farklar_v6b_2.md](gorulen_farklar_v6b_2.md) içinde.

## Araç zinciri ve ikililer

```json
{
  "rustc": "rustc 1.100.0 (0ef1bc386 2026-11-09)",
  "cargo": "cargo 1.100.0 (3acc79c0c 2026-11-05)",
  "c_version": "3.54.1 2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76 (64-bit)",
  "source_parent": "84daa8320688ecac3f7c33c586fc7dfaef37540a",
  "sha256": {
    "/home/hakan/github/sqliters/target/perf-work/bin/sqlite3": "2e6e00f4671f7b7c3adb44438cc3a681ce00ba2373a1ee3c84176661a4130414",
    "/home/hakan/github/sqliters/target/perf-work/v6b2/bin/sqliters": "7ff6d8d8652faa6f3f2d3c9d8211c16510eaebd18615b82ac9c997654ced9a7e",
    "/home/hakan/github/sqliters/target/perf-work/v6b2/bin/speedbench": "00287b565f4ab8ac134561a3681c963967cd5f17d23facfd500b199286388df0",
    "/home/hakan/github/sqliters/target/perf-work/v6b2/bin/rdrv6": "9acb6b93970de946c9ffaf0f8e064889b4ffe88dce57f1b6b0ebb4f737af85cf"
  }
}
```

## Yeniden çalıştırma

```bash
cd /home/hakan/github/sqliters/.claude/worktrees/vdbe
export PATH=/home/hakan/github/sqliters/target/perf-work/bin:$PATH
cargo test --test v6b_pragma_codegen
cargo test --workspace --no-fail-fast
SQLITERS_VDBE_EXEC=1 cargo test --workspace --no-fail-fast
```

Program ve kapsam sürücüleri, SQL girdileri ve bütün sonuçlar $P altında; expdiff6.py önceki V6 aracıdır. res/tests_verified_off.txt, res/tests_verified_on.txt, res/programs.jsonl, res/explain_bugs.json, res/eqp_bugs.json, res/flow_bugs.json, res/dmlfuzz_last_*.json, finder_last/ ve cov/sqlite3.c.gcov ham kanıtı tutar.
