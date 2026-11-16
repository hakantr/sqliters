# V6b.3 — default_cache_size hazırlama ve adımlama eşliği

Başlangıç **f1a2e58**. C SQLite **3.54.1**, Rust **1.100.0**. ileriPlanlama/ girdi olarak kullanılmadı. V6b tamamlanmadı; bu kayıt default_cache_size hazırlama etkisini kapatır.

| C kararı | Koşul | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:519–523` | NeedSchema | Şema hazırlamada okunur. | `crates/sqliters-core/src/engine.rs:1543` |
| `pragma.c:576–582` | RHS var; absInt32(Atoi) | Üreticinin SetCookie P3 değeri i32 sınırları ve önek dönüşümüyle hesaplanır; önbelleğe hemen uygulanır. | `crates/sqliters-codegen/src/pragma.rs:310` |
| `pragma.c:581–582; btree.c:3017–3023` | Cache boyutu ayarı hazırlamada | Normal ve EXPLAIN hazırlama aynı etkiyi yapar; makine anahtarından bağımsızdır. | `crates/sqliters-core/src/lib.rs:5800` |
| `pragma.c:578–579; vdbe.c OP_SetCookie` | Çerez yazımı çalıştırmada | Hazırlanmış operand StmtTxTracker ile eski yazma yoluna aktarılır; adımlama önbellek ayarını tekrar uygulamaz. | `crates/sqliters-core/src/engine.rs:4023` |
| `pragma.c:443–447` | RunOnlyOnce | Yazma tamamlanınca ifade geçersizleşir; reset/yeniden çalıştırma C gibi tekrar hazırlar. | `crates/sqliters-core/src/lib.rs:10357` |
| `pragma.c:553–575` | RHS yok; meta3 pozitif/negatif/sıfır | Çerez adımlamada okunur. Negatif değer mutlak, sıfır -2000 verir; sonuç prepare zamanına dondurulmaz. | `crates/sqliters-core/src/engine.rs:4021` |
| `pragma.c:457,568,579` | TEMP veya bağlı şema | TEMP açılır; önbellek ve çerez aynı seçilmiş şemada tutulur; TEMP için sabit2000/etkisiz ayar yolu kaldırılır. | `crates/sqliters-core/src/lib.rs:5800` |
| `prepare.c:822–836` | C Error / Unsupported | C Error ileti/konumu aktarılır; desteklenmeyen yetkilendirici bağlamı önceki yolda kalır. | `crates/sqliters-core/src/lib.rs:5786` |

Hazırlama ve disk yazımı ayrıdır: default_cache_size=17 hazırlandıktan sonra cache_size=23 hazırlanırsa ilk ifadenin çalışması çereze 17 yazar, önbelleği 23 olarak bırakır. İlk ifade reset edilip tekrar çalıştırılırsa C RunOnlyOnce nedeniyle yeniden hazırlanır ve önbelleği 17 yapar. EXPLAIN önbelleği değiştirir, çerezi yazmaz.

## Doğrulama

```text
statements 4538 | generator accepted 4274: identical 4274, DIFFERENT 0 | unsupported (fallback) 264 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
   unsupported     241  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported      10  PRAGMA page_size prepare effect (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 4538 | generator accepted 4274: identical 4274, DIFFERENT 0 | unsupported (fallback) 264 (of which same as C anyway 246) | not reached (old parser/other) 0 (same 0)
   unsupported     241  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported      10  PRAGMA page_size prepare effect (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 276 | generator accepted 275: identical 275, DIFFERENT 0 | unsupported (fallback) 1 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
{"same": 4272, "unsupported": 246}
```

```text
API lifetime, stdout and file bytes identical in both modes
```

Üç yeni test tests/v6b_pragma_prepare.rs içindedir: hazırlama/step/reset sırası, i32 sınırları ve TEMP, bağlı şema ve geri alma. C API sürücüsüyle aynı sıra iki anahtar durumunda stdout ve dosya baytlarında karşılaştırıldı (res/manual_api.json). Query sonucu prepare sırasında dondurulmaz; fiziksel çerez step anında okunur. Eski farkın kaydı v6b2/res/default_cache_pending_open.json içindedir.

## C kapsamı

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

Bu ölçüm bütün sqlite3Pragma gövdesini, taşınmayan işleyiciler dahil içerir; tamamlanma oranı değildir. Her hiç alınmayan dal ayrı satırda: [karar_esligi_v6b_3_dallar.md](karar_esligi_v6b_3_dallar.md). C kaynak ağacı değiştirilmedi; bağımsız gcov verisi GCOV_PREFIX ile bu adımın dizinine yönlendirildi. Hata enjeksiyonu, özel VFS/authorizer ve tüm hata sonrası durumlar için kapsam genişletilmedi.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1043, 0, 337), açık (1028, 15, 337). Açık 15 başarısız test adları öncekiyle aynı; beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; bütün başarısız seed/sınıf kümeleri aynı. Ayrışma bulucu 245 senaryoda 241 L0, 2 L1, 1 L4, 1 L6; bilinen g06 stderr farkı sürüyor.

CPU6, tmpfs, Rust1.100 tabanına karşı 7 A/B denemesi; anahtar kapalı. Hız ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.21      8.73     0.948x     9.17     8.69     9.60    0.91x  OK
W02_create_indexes                5.65      5.71     1.009x     5.63     5.61     8.76    0.65x  OK
W03_pk_point_select              30.08     29.94     0.996x    29.38    29.34    23.30    1.28x  OK
W04_secondary_idx_range           9.46      9.67     1.022x     9.37     9.52     7.46    1.30x  OK
W05_full_scan_filter_agg          4.40      4.36     0.991x     4.40     4.35    16.57    0.26x  OK
W06_join_group_order             10.79     10.96     1.016x    10.77    10.93    28.23    0.39x  OK
W07_window_functions             37.05     37.47     1.011x    36.97    37.40    50.21    0.75x  OK
W08_indexed_update_delete        11.93     11.98     1.004x    11.90    11.82     7.96    1.51x  OK
W09_like_text_scan               12.59     12.25     0.973x    12.40    12.24    18.92    0.65x  OK
W10_jsonb_extract_agg            13.72     13.81     1.007x    13.64    13.71    10.93    1.26x  OK
W11_fts5_index_and_bm25          69.79     69.72     0.999x    69.00    69.22    57.02    1.22x  OK
W12_rtree_insert_and_bbox        40.48     41.38     1.022x    39.68    39.23    24.63    1.68x  OK
W13_autocommit_wal_txns           4.57      4.62     1.011x     4.41     4.46     4.12    1.12x  OK
W14_cli_sql_script               38.72     38.74     1.001x    38.25    38.56    26.19    1.48x  OK
TOTAL                           298.44    299.34     1.003x   geomean cand/base = 1.000x   digest mismatches = 0
```

## Yerel kanıt ve yeniden çalıştırma

Kök /home/hakan/github/sqliters/target/perf-work/v6b3/. SQL girdileri, API ops dizisi, sürücüler ve ham sonuçlar burada. res/source_manifest.json kaynak SHA256 değerlerini tutar. CLI ikilisi bin/sqliters, program sürücüsü bin/rdrv6. Çalışma alanı testleri C 3.54.1 PATH başında iken cargo test --workspace --no-fail-fast ile, ikinci tur SQLITERS_VDBE_EXEC=1 ile çalıştırıldı.

Üretici DEFAULT_CACHE_SIZE programı zaten taşınmıştı; kabul artışındaki 96 ifade yeni sınama girdileridir. Henüz taşınmayan 21 pragma ve geçmiş koşulları açık; sonraki adım synchronous hazırlama durumu ile pager eşzamanlama bayraklarının C karşılığıdır.
