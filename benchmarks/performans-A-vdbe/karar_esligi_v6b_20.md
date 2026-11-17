# V6b.20 — sayfa1 önbelleği, bozuk hücre ve şema satırı

C SQLite 3.54.1; Rust 1.100. Önceki dilimde açık kalan D120/D121/D122, kaydedilen bütün 640 kısa okuma akışında giderildi. Özgün D118 girdisi de iki anahtarda eşleşir. Genel pager ERROR/E28, TEMP/journal zamanlamaları ve kalan C farkları açıktır.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pager.c:5338–5340,5509–5521,7746–7782` | WAL kilidi ile pager okuyucu durumu ayrı tutulur | Normal WAL bağlantısı fiziksel SHARED kilidi korusa da pager OPEN olabilir. reader_snapshot_held bu ayrımı saklar; WAL sayfa sayısı db_size alanına aktarılır. Sonradan büyüyen WAL kapanırken yeni fiziksel sayfa sayısı okunur. | `crates/sqliters-pager/src/lib.rs:391` |
| `btree.c:3320–3323; pager.c:5552–5699` | SharedLock sonrasında sayfa1 alınır | Ayrı 100 byte ön okuma yerine rollback okuyucusu mevcut pager_get yolunu kullanır. Sayfa1 başlığı doğrulanmadan önce cache ve dbFileVers gerçek sayfayı alır. | `crates/sqliters-pager/src/lib.rs:1643` |
| `pager.c:5483–5523; 3978–3983` | Fiziksel pager sayısı ve mantıksal Btree sayısı | db_size SharedLock fiziksel görüntüsüdür; header page_count ayrı doğrulanır. Fiziksel sayfa sayısı yukarı yuvarlanır. EXCLUSIVE tutulan okuyucuda yeni xFileSize yapılmaz. | `crates/sqliters-pager/src/lib.rs:2978` |
| `btree.c:3406–3432; pager.c:3821–3874` | Başlıktaki sayfa boyutu farklıdır | Eski sayfa bırakılır, cache boyutu ve dbSize güncellenir; gerçek boyutta sayfa1 tekrar alınır. Yeniden doğrulama döngüsü kullanılır. | `crates/sqliters-pager/src/lib.rs:3008` |
| `btree.c:3485–3489; pager.c:5533–5537,5861–5868,2240–2250` | Geçersiz başlıkta son sayfa1 referansı bırakılır | C EXCLUSIVE okuyucuda cache imgesini ve kilidi korur. Rust snapshot_established ile SharedLock hatasını sonraki Btree doğrulama hatasından ayırır; referans sızdırmaz. | `crates/sqliters-pager/src/lib.rs:2862` |
| `pager.c:5469–5483; btree.c:3322` | Cache geçersizleştirme | Dosya sürümü/snapshot değişimi cache’i sıfırlar. Yalnız mantıksal başlık metadata değişikliği, yeni alınmış sayfa1’i ikinci kez atmak için kullanılmaz. | `crates/sqliters-pager/src/lib.rs:3071` |
| `vdbe.c:3215–3222,3393–3401; main.c:1694` | Geçersiz hücre ofseti | C, bu önekte OP_Column kayıt sınırında bozulmayı görür; Rust daha önce hücre ofsetini reddeder. Bu girdide hata kodu CORRUPT ve genel metin hizalanır: database disk image is malformed metni döner. Altı aynı ofset hata yolu hizalandı. | `crates/sqliters-btree/src/page.rs:289` |
| `prepare.c:114–115; 25–55` | Şema satırında rootpage NULL | Eksik record sütunu SQL sorgusunda NULL olur. Rust artık satırı sessizce atmaz; malformed database schema (?) hatası verir. Gerçek isim varsa mesajda o kullanılır. | `crates/sqliters-core/src/schema.rs:1139` |
| `prepare.c:41–43,411–425` | writable_schema ve eksik son SQL sütunu | writable_schema açıkken mevcut schema-error bastırma yolu sürer. Eksik SQL sütunu NULL kabul edilir. Diğer callback/index koşulları tamamlanmış sayılmaz. | `crates/sqliters-core/src/schema.rs:1153` |

## Native C ve ara deneyler

Önceki tam 640 kayıt res/short_matrix_before.json içinde: 16 başarısız vardı. Yalnız cache düzeltmesinden sonra res/short_after_cache.json sekiz başarısız gösterdi; D120’nin sekiz satırı eşleşti. D121 hücre ofseti hata metni ve D122 NULL rootpage denetimi sonrasında res/short_matrix.json 640/640 sonuç, hata, autocommit, rollback callback ve değişiklik sayacı eşliği gösterir. Bütün .db baytları aynı; hata kayıt dosyası boştur.

Ek 128 akış: iki anahtar × normal/exclusive × önceden tutulan/yeni alınan kilit × dört kısa okuma öneki (0,8,16,20) × dört toparlanma bağlamı (xFileSize hatası/shrink; autocommit/savepoint). C’nin geçersiz sayfa1’i exclusive cache’te tutması, normal okuyucunun dosyayı yeniden yoklaması, IOERR_FSTAT’ın işlem geri alma kararı ve shrink sonrası doğru veri yeniden yüklemesi karşılaştırıldı. Sonuç/durum/sayaçlar ve bütün dosya baytları aynı (res/cache_matrix.json).

Başlangıçta yeni pager testi, exclusive kip ilk defa kilit alındığında hatalı xFileSize çağrısını yakaladı (res/pager_final.txt). SharedLock başarı aşaması ayrı izlenince test geçti (res/pager_phase_fixed.txt; 18/18 pager testi). Bu ayrım yalnız daha önce exclusive kilit tutulmuş örneklerle örtülmedi; native matrisin yarısı ilk kilidi hata sırasında alır.

İlk WAL birim testi, full page okuması öne alınınca henüz WAL açılmadan hata aldı ve wal.is_some beklentisi geçersiz kaldı (res/pager_cache_initial.txt). WAL temizlemesi testi artık önce gerçek WAL snapshot’ını açar; mevcut WAL’ın korunduğunu sonra sınar. Bu kurulum değişikliği, açılmamış WAL işaretçisinin varlığını C garantisi olarak sunmaz.

C’nin CONFIG_LOG tanı günlüğü res/cell_error_C_log.json içinde tutuldu: önek 108 OP_Column corrupt çıkışına, önek 512 prepare.c:53 corruptSchema çıkışına gider. Rust, ofset0 girdisini daha önce reddeder; genel bozuk kayıt denetim sırası birebir kanıtlandı sayılmadı. Bu girdide her iki yolun sonuç kodu/metni ve G/Ç izi aynıdır. Bu sabit ofset0 örneğinde SQLite imzası C’ye payload=83, rowid=81, header=76 ve ilk serial type=105 verir; 105’in uzunluğu 46 olduğundan 76+46=122>83 koşulu vdbe.c:3215–3222’de zorunlu olarak CORRUPT üretir (uzunluk formülü vdbeaux.c:4003–4012). Rust aynı sonucu hücre ofseti guard’ında daha önce üretir. Bu çıkarım yalnız kaydedilen ofset0 ailesi içindir.

İlk tam test koşusu WAL→DELETE gerilemesini yakaladı: WAL’ı yalnız okumuş bağlantıda db_size aktarımı eksikti; fiziksel SHARED kilidi de yanlışlıkla tutulan pager okuyucusu sayılıyordu. Eski tam sonuçlar res_before_wal_snapshot_fix/ içinde sabittir; bu sürüm commit edilmedi. reader_snapshot_held ve WAL sayacı aktarımı sonrası hedef test iki anahtarda geçti. Yeni dört native akış, ikinci bağlantının beklemesi ve dosyayı sonradan büyütmesi durumlarını içerir: önce res/wal_reader_exit_before.json farklı, sonra res/wal_reader_exit.json sonuç/hata/dosya baytları aynı. Dört kapı son kaynak SHA’sıyla baştan çalıştırıldı.

Özgün D118 C ham çıktısı ve dosya özeti öncekiyle aynı girdi üzerinde denetlendi. Son Rust sonucu aynı 26/26 file is not a database; 100 byte ek ön okuma bu rollback akışından kaldırıldı (res/short_original_after.json). Sınırlı header_probe yolu ve aktif WAL başlık yolu ayrıca 100 byte okuyabilir; bütün pager yolları yeniden taşındı sayılmaz.

72 G/Ç gözleminde veritabanı sayfa okumaları ve bütün dosya baytları aynı. Ham izin tamamı 52/72 eşleşir (önce 14/72); kalan 20 dirty/TEMP örneğinde journal ön imgeleri ve TEMP cache yaşamı farklıdır. Bu kayıtlar eşit sayılmadı. D117 30 hata akışının sonuç/durum/dosya eşliği ve önceki iki bağlantı auto_vacuum, raw-version, 896 sync, 160 bağlantı/cache, 16 cold-cache, reopen ve statement-status deneyleri korundu. D119 56 yürütme, 16 reprepare ve 64 callback/sayaç akışı da yeniden eşleşti.

Tam program 13181 aynı; 126 Unsupported başarı sayılmadı. EXPLAIN 15632, EQP 15632, yürütme 11459 aynı. API 4157/4158 aynı; tek çıktı farkı eski D103. Bütün API dosya baytları aynı.

## C kapsamı

Ölçüm birikimlidir: kabul edilen SQL/şema kurulumları, önceki native kanıtlar ve yeni 640+128 akış. Yeni seçili tam işlevler fiziksel sayfa sayısı, normal pager_get, sayfa1 unref/son referans temizleme ve schema callback/error yordamlarıdır. Tam bir işlevin ölçülmesi, bu işlevin bütün Rust karşılığının kanıtlandığı anlamına gelmez; alınmayan yönler ayrı sınıflandırılır. sqlite3VdbeExec/Halt/step ve SELECT üst kuralı yalnız belirtilen bloklarda sayılır.

### Seçili C işlevlerinin birikimli kapsamı (gcov, -O0 başvuru derlemi)

Corpus: the 15757 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C dosyası | Kapsanan satırlar | En az bir kez alınan dallar |
|---|---|---|
| pragma.c | 870 / 1142 (76.2%) | 577 / 794 (72.7%) |
| btree.c | 249 / 276 (90.2%) | 110 / 136 (80.9%) |
| pager.c | 315 / 376 (83.8%) | 206 / 300 (68.7%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 84 / 95 (88.4%) | 40 / 50 (80.0%) |
| vdbeaux.c | 57 / 64 (89.1%) | 14 / 18 (77.8%) |
| callback.c | 25 / 26 (96.2%) | 5 / 6 (83.3%) |
| prepare.c | 195 / 259 (75.3%) | 121 / 192 (63.0%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 82 / 94 (87.2%) | 68 / 82 (82.9%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| vdbeapi.c | 6 / 15 (40.0%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| **total** | 2115 / 2651 (79.8%) | 1251 / 1756 (71.2%) |

| C işlevi | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 72.6 | 675 | 70.1 | 117 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerSetFlags | pager.c | 25 | 100.0 | 18 | 100.0 | 0 |
| sqlite3PagerSetPagesize | pager.c | 31 | 93.5 | 26 | 76.9 | 6 |
| sqlite3BtreeSetPageSize | btree.c | 23 | 100.0 | 22 | 100.0 | 0 |
| sqlite3BtreeGetPageSize | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |
| getAutoVacuum | pragma.c | 6 | 100.0 | 10 | 100.0 | 0 |
| sqlite3BtreeSetAutoVacuum | btree.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3BtreeGetAutoVacuum | btree.c | 7 | 100.0 | 4 | 100.0 | 0 |
| btreeEndTransaction | btree.c | 15 | 100.0 | 8 | 87.5 | 1 |
| sqlite3BtreeCommitPhaseTwo | btree.c | 15 | 86.7 | 8 | 62.5 | 1 |
| sqlite3BtreeRollback | btree.c | 22 | 95.5 | 14 | 78.6 | 3 |
| sqlite3BtreeSavepoint | btree.c | 16 | 93.8 | 14 | 64.3 | 3 |
| lockBtree | btree.c | 73 | 75.3 | 44 | 72.7 | 11 |
| newDatabase | btree.c | 24 | 100.0 | 4 | 75.0 | 1 |
| setAllPagerFlags | pragma.c | 10 | 100.0 | 6 | 100.0 | 0 |
| sqlite3BtreeSetSpillSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3PagerSetSpillsize | pager.c | 2 | 100.0 | 0 | 100.0 | 0 |
| numberOfCachePages | pcache.c | 6 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PcacheSetSpillsize | pcache.c | 8 | 100.0 | 6 | 100.0 | 0 |
| sqlite3_busy_timeout | main.c | 8 | 100.0 | 2 | 100.0 | 0 |
| sqlite3_busy_handler | main.c | 8 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_wal_autocheckpoint | main.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3VdbeList | vdbeaux.c | 41 | 82.9 | 16 | 75.0 | 4 |
| sqlite3SetTextEncoding | callback.c | 5 | 100.0 | 0 | 100.0 | 0 |
| sqlite3ExpirePreparedStatements | vdbeaux.c | 4 | 100.0 | 2 | 100.0 | 0 |
| sqlite3InitOne | prepare.c | 92 | 85.9 | 58 | 79.3 | 9 |
| getLockingMode | pragma.c | 5 | 100.0 | 6 | 100.0 | 0 |
| returnSingleText | pragma.c | 5 | 100.0 | 2 | 50.0 | 1 |
| sqlite3PagerLockingMode | pager.c | 4 | 100.0 | 6 | 83.3 | 1 |
| pragmaVtabConnect | pragma.c | 34 | 82.3 | 12 | 58.3 | 5 |
| pragmaVtabBestIndex | pragma.c | 27 | 88.9 | 14 | 78.6 | 3 |
| pragmaVtabFilter | pragma.c | 26 | 84.6 | 14 | 71.4 | 4 |
| pragmaVtabColumn | pragma.c | 7 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PragmaVtabRegister | pragma.c | 5 | 100.0 | 4 | 50.0 | 2 |
| pagerUnlockDb | pager.c | 8 | 100.0 | 6 | 83.3 | 1 |
| pager_incr_changecounter | pager.c | 11 | 100.0 | 8 | 62.5 | 3 |
| pager_write_changecounter | pager.c | 6 | 100.0 | 2 | 50.0 | 1 |
| pager_write_pagelist | pager.c | 25 | 100.0 | 24 | 83.3 | 3 |
| pagerPlaybackSavepoint | pager.c | 37 | 67.6 | 44 | 45.5 | 13 |
| sqlite3PagerRollback | pager.c | 18 | 100.0 | 16 | 75.0 | 4 |
| sqlite3RunParser | tokenize.c | 75 | 77.3 | 54 | 68.5 | 10 |
| sqlite3Prepare | prepare.c | 68 | 58.8 | 46 | 52.2 | 13 |
| sqlite3Error | util.c | 6 | 100.0 | 4 | 75.0 | 1 |
| sqlite3ErrorClear | util.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3ErrorMsg | util.c | 18 | 77.8 | 6 | 50.0 | 2 |
| getTempStore | pragma.c | 8 | 100.0 | 8 | 100.0 | 0 |
| invalidateTempStorage | pragma.c | 11 | 100.0 | 6 | 100.0 | 0 |
| changeTempStorage | pragma.c | 8 | 100.0 | 4 | 100.0 | 0 |
| sqlite3ResetAllSchemasOfConnection | build.c | 14 | 92.9 | 8 | 75.0 | 2 |
| sqlite3SchemaClear | callback.c | 21 | 95.2 | 6 | 83.3 | 1 |
| sqlite3VtabCallConnect | vtab.c | 18 | 22.2 | 6 | 16.7 | 3 |
| vtabCallConstructor | vtab.c | 70 | 75.7 | 42 | 52.4 | 13 |
| sqlite3VtabEponymousTableInit | vtab.c | 30 | 80.0 | 12 | 50.0 | 4 |
| sqlite3Init | prepare.c | 13 | 100.0 | 12 | 100.0 | 0 |
| sqlite3ReadSchema | prepare.c | 11 | 100.0 | 6 | 83.3 | 1 |
| sqlite3MultiValues | insert.c | 61 | 72.1 | 32 | 46.9 | 13 |
| sqlite3VdbeSwap | vdbeaux.c | 19 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_stmt_status | vdbeapi.c | 15 | 40.0 | 4 | 75.0 | 1 |
| sqlite3_limit | main.c | 13 | 84.6 | 12 | 66.7 | 2 |
| sqlite3DecOrHexToI64 | util.c | 14 | 100.0 | 16 | 93.8 | 1 |
| sqlite3Atoi64 | util.c | 51 | 84.3 | 54 | 83.3 | 6 |
| sqlite3_db_release_memory | main.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerShrink | pager.c | 3 | 100.0 | 0 | 100.0 | 0 |
| sqlite3PcacheShrink | pcache.c | 3 | 100.0 | 0 | 100.0 | 0 |
| pcache1Shrink | pcache1.c | 9 | 100.0 | 2 | 100.0 | 0 |
| pcache1EnforceMaxPage | pcache1.c | 10 | 100.0 | 8 | 100.0 | 0 |
| sqlite3PagerSharedLock | pager.c | 60 | 71.7 | 58 | 67.2 | 13 |
| readDbPage | pager.c | 19 | 100.0 | 12 | 91.7 | 1 |
| pager_reset | pager.c | 5 | 100.0 | 0 | 100.0 | 0 |
| sqlite3RollbackAll | main.c | 23 | 100.0 | 18 | 100.0 | 0 |
| sqlite3CloseSavepoints | main.c | 9 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerReadFileheader | pager.c | 8 | 100.0 | 4 | 100.0 | 0 |
| decodeFlags | btree.c | 47 | 89.4 | 10 | 90.0 | 1 |
| sqlite3ErrStr | main.c | 18 | 50.0 | 10 | 40.0 | 2 |
| pagerPagecount | pager.c | 12 | 91.7 | 8 | 75.0 | 2 |
| sqlite3PagerPagecount | pager.c | 3 | 100.0 | 0 | 100.0 | 0 |
| getPageNormal | pager.c | 45 | 77.8 | 32 | 65.6 | 10 |
| sqlite3PagerUnrefPageOne | pager.c | 5 | 100.0 | 0 | 100.0 | 0 |
| pagerUnlockIfUnused | pager.c | 4 | 100.0 | 2 | 100.0 | 0 |
| pagerUnlockAndRollback | pager.c | 21 | 42.9 | 14 | 71.4 | 3 |
| sqlite3InitCallback | prepare.c | 57 | 70.2 | 56 | 48.2 | 20 |
| corruptSchema | prepare.c | 18 | 66.7 | 14 | 50.0 | 5 |
| sqlite3PagerCloseWal | pager.c | 19 | 63.2 | 18 | 27.8 | 6 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 93186 | 2/2 |
| parse.y:524 | 46544 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen dört işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_Init | 2/20 |

## V6b.20 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeapi.c:990–1023 | 14/17 | 7/10 |
| vdbe.c:3136–3147 | 0/2 | 0/4 |
| vdbe.c:3210–3224 | 7/7 | 10/12 |
| vdbe.c:3393–3402 | 4/6 | 1/2 |


561 alınmayan dal [karar_esligi_v6b_20_dallar.md](karar_esligi_v6b_20_dallar.md) içinde ayrı satırlarda sınıflandırıldı. Mmap, tahsis, pager ERROR, diğer callback/index doğrulamaları ve kalan özel hatalar açık kapsam eksikleridir.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1113, 0, 337), açık (1098, 15, 337). Açık anahtarda önceki 15 başarısızın adları aynı. DML kapalı 378/400, açık 398/400; sınıf/seed kümeleri aynı. Beklentiler değiştirilmedi.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı; CPU6/tmpfs; yedi A/B; anahtar kapalı. Performans ayarı yapılmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.21      8.80     0.955x     9.19     8.72     9.60    0.92x  OK
W02_create_indexes                5.68      5.65     0.995x     5.63     5.64     8.76    0.65x  OK
W03_pk_point_select              30.03     29.98     0.999x    29.45    29.57    23.30    1.29x  OK
W04_secondary_idx_range           9.41      9.72     1.033x     9.29     9.58     7.46    1.30x  OK
W05_full_scan_filter_agg          4.40      4.31     0.981x     4.39     4.29    16.57    0.26x  OK
W06_join_group_order             10.79     11.14     1.033x    10.74    11.13    28.23    0.39x  OK
W07_window_functions             37.10     37.92     1.022x    37.02    37.82    50.21    0.76x  OK
W08_indexed_update_delete        11.97     12.00     1.002x    11.93    11.74     7.96    1.51x  OK
W09_like_text_scan               12.48     12.55     1.005x    12.41    12.52    18.92    0.66x  OK
W10_jsonb_extract_agg            13.75     13.62     0.991x    13.69    13.52    10.93    1.25x  OK
W11_fts5_index_and_bm25          69.19     70.72     1.022x    68.78    69.28    57.02    1.24x  OK
W12_rtree_insert_and_bbox        40.10     42.49     1.060x    39.43    40.10    24.63    1.73x  OK
W13_autocommit_wal_txns           4.56      4.67     1.023x     4.42     4.51     4.12    1.13x  OK
W14_cli_sql_script               38.86     52.89     1.361x    38.51    52.61    26.19    2.02x  OK
TOTAL                           297.53    316.46     1.064x   geomean cand/base = 1.031x   digest mismatches = 0
```

## Sıradaki adım ve kayıt

V6b’nin kalan PRAGMA işleyicilerine dönülür; sırada database_list. Genel başlık kabul kuralları, bozuk kayıt hata metinleri, D103, E28 pager ERROR/COMMIT, M4, UTF-16/geçersiz UTF-8, eski yürütücü ve hızlı yollar hâlâ açıktır. Bu dilim tam C eşliği veya yayın tamamlanması iddiası değildir.

Kanıt `/home/hakan/github/sqliters/target/perf-work/v6b20/`; üç değişen kaynak res/source_manifest.json ile sabitlendi. C kaynakları değiştirilmedi. ileriPlanlama değerlendirilmedi.
