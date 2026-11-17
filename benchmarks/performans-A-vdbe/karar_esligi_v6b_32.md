# V6b.32 — mmap hazırlama etkisi ve TEMP dosya ömrü

MMAP_SIZE üreticiye ve hazırlama sırasında çalışan host/VFS sınır köprüsüne taşındı. Bellek/açılmamış TEMP için yanlış 0 satırı giderildi. Bu aktarımın önkoşulu olan TEMP dosyasının ilk yazmada açılması ve her commit’te dökülmesi düzeltildi. **mmap işi tamamlandı sayılmaz:** açık mapped-page referansıyla limit değiştiren altı akış D142 olarak açık. Hız ayarı uygulanmadı.

## C karar eşliği

| C koşulu | Ayrım | Sonuç | Rust yeri |
|---|---|---|---|
| pragma.c:951–987 | MMAP_SIZE programı ve hazırlama etkisi | Sınır hazırlamada değişir; sonucun integer/boş biçimi programda donar. SQLITERS_VDBE_EXEC kapalı/açık aynı C makinesini kullanır; step ayarı ikinci kez uygulamaz. | `crates/sqliters-codegen/src/pragma.rs:278` |
| pragma.c:956–960 | Ondalık/hex dönüşümü | sqlite3DecOrHexToI64 dönüş durumu C gibi göz ardı edilir. Negatif sonuç hedef global szMmap=0’a döner; metin öneği/taşma değeri korunur. | `crates/sqliters-codegen/src/pragma.rs:279` |
| pragma.c:961–966 | Şema adı / niteliksiz istek | Niteliksiz istek bağlantı varsayılanını ve ters sırayla bütün açık Btree’leri günceller. Nitelikli istek yalnız seçili Btree’yi etkiler; kapalı TEMP atlanır. | `crates/sqliters-core/src/lib.rs:5971` |
| btree.c:2713–2716,3051–3057 | Sonradan ATTACH | BtreeOpen yeni pager’a bağlantının szMmap isteğini aktarır. Adlı dosyada VFS sınırı da ayarlanır; TEMP fd kapalı olduğundan yalnız pager isteği saklanır. | `crates/sqliters-core/src/db_set.rs:1942` |
| build.c:5404–5429 | Yeni TEMP Btree | sqlite3OpenTempDatabase altındaki BtreeOpen szMmap isteğini devralır, sonra nextPagesize uygulanır. Sonradan pagerOpentemp ile açılan VFS fd başlangıçta yine global varsayılan 0’dır; bu iki değer birbirine karıştırılmaz. | `crates/sqliters-core/src/db_set.rs:1386` |
| pager.c:3587–3606 | PagerSetMmapLimit / pagerFixMaplimit | Pager isteği saklanır. Açık ve denetimi destekleyen dosyaya hint gönderilir; hint hata sonucu C gibi yutulur. Dosyası olmayan TEMP’de yalnız istek saklanır. | `crates/sqliters-pager/src/lib.rs:2615` |
| main.c:4224–4275; os.c:129–159 | File-control sorgusu | mmap_size VFS yöntemi None ile NOTFOUND verir; bellek/açılmamış dosya sonucunda program ResultRow içermez. Sorgu hatası Parse rc/nErr’ye taşınır; genel özel VFS hata enjeksiyonu ayrıca açık. | `crates/sqliters-core/src/cgen.rs:110` |
| os_unix.c:4228–4244 | Dosya sınırı ve eski değeri | OS dosyası önceki sınırı döndürür. Yeni sıfır/pozitif sınır hedef mxMmap=0x7fff0000 ile kısılır; negatif girdi yalnız sorgular. Windows/32-bit/başka global config profilleri bu Linux 64-bit ölçümün kanıtı değildir. | `crates/sqliters-vfs/src/lib.rs:2128` |
| os_unix.c:4243–4248 | Açık mmap referansı ve harita yenileme | **Açık eksik (D142):** C nFetchOut!=0 iken yeni limiti kabul etmez. Rust VFS henüz gerçek mapped-page referansı/harita ömrü taşımaz; altı native akış bu nedenle farklıdır. Bu dalın eşliği iddia edilmez. | `crates/sqliters-vfs/src/lib.rs:2128` |
| pragma.c:974–977 | NOTFOUND / diğer hata | NOTFOUND boş sonuçtur. Diğer dosya-denetim hatası rc/nErr ile hazırlamayı düşürür. Bu hata yönü ölçümde alınmadı; hata enjeksiyonu eksik olarak ayrı sınıflandırıldı. | `crates/sqliters-codegen/src/pragma.rs:293` |
| pragma.c:484–504; main.c:4224 | EXPLAIN ve açılmamış TEMP | EXPLAIN kapalı TEMP Btree’yi açmaz. Temel file-control Btree seçimi SQL logic error verir; cold_temp_explain.json iki CLI’da aynı hatayı kaydeder. Normal PRAGMA TEMP Btree’yi açar, VFS dosyasını açmaz. | `crates/sqliters-codegen/src/pragma.rs:257` |
| pager.c:4493–4502 | TEMP fiziksel dosyasını açma | begin_write fiziksel dosya açmaz. Gerçek dirty liste ilk defa pager_write_pagelist’e ulaşınca dosya açılır. pagerOpentemp bekleyen mmap isteğini yeni VFS dosyasına otomatik uygulamaz. | `crates/sqliters-pager/src/lib.rs:2004` |
| pager.c:2030–2035,6550–6562 | TEMP commit flush kararı | Dosyası olmayan TEMP commit’i sayfaları diske dökmez. Açık dosyada dirty yüzde 25 altındaysa yine dökmez. Değişmemiş WRITER_LOCKED işlemi erken biter. Backuplar C’nin restart yoluna bildirilir. | `crates/sqliters-pager/src/lib.rs:4866` |
| pcache.c:907–912 | Dirty yüzdesi | nDirty*100 / numberOfCachePages; mevcut pagecount’a göre oranlanmaz. nCache=0 için sıfır döner; bu özel sıfır yönü bu ölçümde alınmadı. | `crates/sqliters-pager/src/pcache.rs:992` |
| pager.c:2175–2187; pcache.c:641–648 | TEMP dirty/writable ömrü | Commit/rollback ardından önbellekte kalan TEMP sayfaları temizlenmez. NEED_SYNC ve WRITEABLE silinir, dirty liste ve pSynced korunur; sonraki işlem eski sayfaları yeniden journal’a alır. | `crates/sqliters-pager/src/pcache.rs:981` |
| pager.c:4663–4738,4493–4502 | Okurken TEMP spill | Commit’ten kalan dirty TEMP sayfası okuma sırasında da spill olabilir; yalnız Write durumuna izin veren eski Rust guardı kaldırıldı. Diğer pager türlerinin eski guardı korunur. | `crates/sqliters-pager/src/lib.rs:1734` |
| pager.c:1716,3008–3009,3873–3877 | Journal playback kodlama/sayfa boyutu dönüşü | Rollback sayfa geometrisini geri yükleyen C PagerSetPagesize aynı zamanda mmap hint’ini yeniden gönderir. Bu yüzden daha önce açılmış TEMP dosyasının 0 limiti rollback’ten sonra bekleyen isteğe dönüşebilir. | `crates/sqliters-pager/src/lib.rs:5218` |
| pager.c:3484–3499,3526–3535 | İşlem savepoint’i | Dış işlem savepoint’ine rollback pSavepoint=NULL ile ana journal başından oynatılır; başlık okumasının mmap hint etkisi bu yolda da korunur. Genel savepoint journal/hata kapsamının tamamı taşındı sayılmaz. | `crates/sqliters-pager/src/lib.rs:5553` |

## Doğrudan karşılaştırmalar

- **250 mmap API akışı:** 240 tam aynı. Dört eski `CREATE TABLE t` hata-offset farkı (C 13, Rust -1) korunur; altı açık mapped-reader farkında C 1048576 sınırını korurken Rust 0 kabul eder. Hiçbiri normalize edilerek başarılı sayılmaz. `mmap_known.py` yalnız bu açık farkların tam biçimini sınırlar; ham JSON karşılaştırması başarısız kaydı tutar. Tüm 250 akışın ana/aux dosya SHA-256 değerleri aynıdır.
- **270 TEMP akışı tamamen aynı:** iki anahtar, üç kodlama, bellek/disk/özel geçici main, cache_size 2/10/100, autocommit/rollback/savepoint/read+shrink/VACUUM temp; veri, hata, sayaç, mmap sonucu ve kalıcı dosyalar birlikte kıyaslandı. Anonim TEMP dosyasının ham byte dökümü bu matrisin kanıtı değildir; bu konu E27/genel pager doğrulamasında açık tutulur.
- **144 gerçek mmap programı** 14 başlık alanı, bütün opcode/P4 ve altprogram biçimiyle aynı. Query/set, hex/ondalık/taşma/kısmi metin, snapshot ve açılmamış/yazılmış küçük TEMP örnekleri normal API’nin ilk step’inde gözlendi.
- **78 ek ifade** EXPLAIN, EQP ve doğrudan yürütmede aynı. Yeni mmap derlemi dahil 13529 genel program aynı, 61 Unsupported; önceki 1116 LIKE ve 528 bound tam program kanıtları korundu.
- İki public regresyon testi eklendi: hazırlama etkisi/snapshot/diskin 0-byte kalması; bellek ve unspilled TEMP’nin boş sonucu, spill sonrası 0 VFS limiti, rollback’in bekleyen limiti aktarması ve veri korunumu.

## Test beklentileri — değişiklik yapılmadı

Kapalı **(1128, 4, 337)**, açık **(1114, 18, 337)** (geçti, başarısız, ignored). V6b.31’in başarısız isimlerine yalnız aşağıdaki üç test eklendi. C dışında kalan beklentiler eski yürütücü kaldırılana kadar korunur; bunlar yeşil test olarak raporlanmaz.

| Kayıt | Test | Beklenti / C kanıtı |
|---|---|---|
| F10 | c_parity_tests::pragmas_that_report_a_setting_return_one_row | Bellekte mmap_size için Some(0) bekler; hedef MAX_MMAP_SIZE>0 C derlemi NOTFOUND nedeniyle satır vermez. 144 program + 250 API kaydı ve cold TEMP EXPLAIN sonucu korunur. |
| F11 | test_database_kind_named_private_temporary_and_memory_lifecycle | begin_write sonrası bir TEMP xOpen bekler. Aynı C VFS gözlemcisi private_open=0, private_begin_write=0, private_small_commit=0 gösterir. |
| F12 | test_gate2_5_multidb_pragmas_and_vacuum_into | Yazılmış küçük TEMP için unlocked tanısı bekler. C SQLITE_FCNTL_LOCKSTATE rc=12 (NOTFOUND), çıktı argümanı -99 değişmez; Rust pager tanısı None olur. Hedef C derlemi PRAGMA lock_status sağlamaz; Rust’ın legacy unknown metni ayrı bir SQL eşliği iddiası değildir. |

C gözlemcisi `temp_expectation_probe.c`, aynı coverage SQLite nesnesine bağlı delegating xOpen izleyicisidir. `res/temp_expectation_C.json` stderr boş ve exit=0 olarak doğrulandı. Eski F4/F6–F9 sınıfları korundu.

## Dört kapı

DML **378/400** ve **398/400**, kusur sınıf/örnekleri V6b.31 ile aynı. Finder stdout **245/245**, stderr **244/245**; eski iki referans sınıf farkı aynı. Genel EXPLAIN/EQP **16027/16027**, yürütme **11789** aynı; ek mmap 78/78 her üç biçimde aynı. Defer matrisinde eski 20 D52, manual API’de tek D103 açık.

Son sabit kaynak ve Rust 1.100 tabanı için yedi denemede toplam **1.224x**, W09 **4.955x**, W14 **1.375x**; 14 C digest farkı **0**. Ham kayıt speedbench.txt içindedir. TEMP devralma düzeltmesinden önceki ağacın 1.311x ve 1.225x ölçümleri res_before_inheritance altında korunur; son kaynak ölçümü olarak kullanılmaz. Performans ayarı yapılmadı; bu resmi son ölçüm değildir.

## Kapsam ve açık işler

Ana üretici/PRAGMA tablosunda **736 alınmayan dal**, [ayrı dosyada](karar_esligi_v6b_32_dallar.md) sınıflandırılır. Yeni MMAP/TEMP tablosu **68 dal** ve **6 alınmayan yön** içerir; [C koşulu, gcov sonucu ve Rust yeri](karar_esligi_v6b_32_mmap_dallar.md) ayrı gösterilir. Ana tabloyla örtüşen noktalar vardır; sayılar benzersiz dal toplamı değildir. Ölçülmüş olsa da nFetchOut/harita yenileme karşılığı eksik yön D142 olarak özellikle işaretlendi.

**D142 açık:** mapped-page lease, nFetchOut guardı ve gerçek mmap/unmap yaşamı tamamlanmadı. mmap_size için bütün C eşliği iddia edilmez. Özel VFS iVersion<3, file-control hatası, NOMEM ve anonim TEMP byte dökümü ayrıca kanıtlanmadı. **D144** dört eski DDL hata-offset farkıdır. **D143 / D57** module_list’in kayıt sırası/lazy JSON modülleri yoklamasında görülen eski farklar ayrı başlangıç dosyasında tutulur; bu adımda ele alınmadı. E27’nin yalnız TEMP dosya açılışı/commit dirty kararına dokunuldu; genel cache spill zamanlaması kapanmış sayılmaz. E28 pager ERROR ve diğer V6b/M4 kusurları sürer.

Bir sonraki doğruluk işi, D142’nin açık okuyucu/mapped-page yaşamını karşılamak; ardından kalan PRAGMA işleyicileridir. Tam C işlevselliği/yayın garantisi verilmedi.

## Kanıt dosyaları

Dizin `/home/hakan/github/sqliters/target/perf-work/v6b32`. Kaynak/binary hash’leri, mmap_final/cov, temp_final/cov, mmap_full_programs, mmap_explain/eqp/exec, tests_off/on, DML/finder ve son kaynağın hız kaydı saklanır. Önceki ağaç res_before_inheritance altında ayrı tutulur. İlk özel harness denemelerindeki göreli ikili yolu, coverage link/checksum ve boş ops satırı sorunları final sonuçlara katılmadı; son geçerli koşular yeniden doğrulandı. Boş ops satırındaki C yardımcısı davranışı SQLite sonucu olarak sayılmadı.

```json
{
  "crates/sqliters-codegen/src/pragma.rs": "601c9e20385c358bc7fd729ff408710f68025ae11830d87418c551fffa0ed177",
  "crates/sqliters-codegen/src/sqliteint.rs": "305670faecfeee370e341e00e75dba4a17c0f8caf20a83b60731c84d70f971a8",
  "crates/sqliters-core/src/cgen.rs": "70f5a1a773f66f156f1a5a597c8b602554a1898dca2181debc59eac739486d28",
  "crates/sqliters-core/src/db_set.rs": "75a42b46b0ff3accf5d6a07d129f4a9f02edcf81db53952aaecfc4bb7d802f88",
  "crates/sqliters-core/src/lib.rs": "7cef1e9b00a8aa1486295961428cfbc5a94c926a9685606140c65562319daad9",
  "crates/sqliters-pager/src/lib.rs": "38e225361471492d6d4da449d416c4258e0e01faf257d3c8822fcdf30f2be720",
  "crates/sqliters-pager/src/pcache.rs": "627f9c3cd8212df783ea0132dda98bd6bb232fff0a414556d0228647ddfdfff8",
  "crates/sqliters-vfs/src/lib.rs": "dbeb78909429d59126d492f9f7391dd0991151ba78d9759c7221afceac13390c",
  "tests/v6b_mmap_prepare.rs": "f847c909c64cd4f96c75cfe241a35f25b047cc4490a08d864696d392bcffd072"
}

```

## Ana C kapsam ölçümü

# C-side coverage of the functions ported or completed in step V6b.32 (gcov, -O0 build of the reference amalgamation)

Corpus: the 16152 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 925 / 1142 (81.0%) | 620 / 794 (78.1%) |
| btree.c | 251 / 278 (90.3%) | 112 / 136 (82.4%) |
| pager.c | 329 / 380 (86.6%) | 229 / 306 (74.8%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 128 / 142 (90.1%) | 69 / 86 (80.2%) |
| vdbeaux.c | 115 / 142 (81.0%) | 46 / 68 (67.6%) |
| callback.c | 70 / 74 (94.6%) | 34 / 38 (89.5%) |
| prepare.c | 206 / 259 (79.5%) | 131 / 192 (68.2%) |
| tokenize.c | 59 / 76 (77.6%) | 37 / 54 (68.5%) |
| util.c | 93 / 97 (95.9%) | 77 / 84 (91.7%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 46 / 61 (75.4%) | 17 / 32 (53.1%) |
| vdbeapi.c | 10 / 19 (52.6%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| vdbemem.c | 31 / 34 (91.2%) | 26 / 36 (72.2%) |
| utf.c | 51 / 64 (79.7%) | 63 / 74 (85.1%) |
| hash.c | 74 / 74 (100.0%) | 42 / 44 (95.5%) |
| expr.c | 20 / 22 (90.9%) | 16 / 24 (66.7%) |
| ctime.c | 3 / 3 (100.0%) | 0 / 0 (0.0%) |
| func.c | 91 / 121 (75.2%) | 88 / 130 (67.7%) |
| fkey.c | 212 / 273 (77.7%) | 120 / 189 (63.5%) |
| whereexpr.c | 58 / 71 (81.7%) | 51 / 80 (63.8%) |
| **total** | 2902 / 3500 (82.9%) | 1836 / 2459 (74.7%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 77.9 | 675 | 75.7 | 102 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerSetFlags | pager.c | 25 | 100.0 | 18 | 100.0 | 0 |
| sqlite3PagerSetPagesize | pager.c | 31 | 93.5 | 26 | 80.8 | 5 |
| sqlite3BtreeSetPageSize | btree.c | 23 | 100.0 | 22 | 100.0 | 0 |
| sqlite3BtreeGetPageSize | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |
| getAutoVacuum | pragma.c | 6 | 100.0 | 10 | 100.0 | 0 |
| sqlite3BtreeSetAutoVacuum | btree.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3BtreeGetAutoVacuum | btree.c | 7 | 100.0 | 4 | 100.0 | 0 |
| btreeEndTransaction | btree.c | 15 | 100.0 | 8 | 87.5 | 1 |
| sqlite3BtreeCommitPhaseTwo | btree.c | 15 | 86.7 | 8 | 62.5 | 1 |
| sqlite3BtreeRollback | btree.c | 22 | 95.5 | 14 | 78.6 | 3 |
| sqlite3BtreeSavepoint | btree.c | 16 | 93.8 | 14 | 78.6 | 3 |
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
| sqlite3InitOne | prepare.c | 92 | 89.1 | 58 | 81.0 | 8 |
| getLockingMode | pragma.c | 5 | 100.0 | 6 | 100.0 | 0 |
| returnSingleText | pragma.c | 5 | 100.0 | 2 | 50.0 | 1 |
| sqlite3PagerLockingMode | pager.c | 4 | 100.0 | 6 | 83.3 | 1 |
| pragmaVtabConnect | pragma.c | 34 | 94.1 | 12 | 83.3 | 2 |
| pragmaVtabBestIndex | pragma.c | 27 | 88.9 | 14 | 78.6 | 3 |
| pragmaVtabFilter | pragma.c | 26 | 88.5 | 14 | 78.6 | 3 |
| pragmaVtabColumn | pragma.c | 7 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PragmaVtabRegister | pragma.c | 5 | 100.0 | 4 | 75.0 | 1 |
| pagerUnlockDb | pager.c | 8 | 100.0 | 6 | 83.3 | 1 |
| pager_incr_changecounter | pager.c | 11 | 100.0 | 8 | 62.5 | 3 |
| pager_write_changecounter | pager.c | 6 | 100.0 | 2 | 50.0 | 1 |
| pager_write_pagelist | pager.c | 25 | 100.0 | 24 | 87.5 | 3 |
| pagerPlaybackSavepoint | pager.c | 37 | 91.9 | 44 | 75.0 | 9 |
| sqlite3PagerRollback | pager.c | 18 | 100.0 | 16 | 81.2 | 3 |
| sqlite3RunParser | tokenize.c | 75 | 77.3 | 54 | 68.5 | 10 |
| sqlite3Prepare | prepare.c | 68 | 63.2 | 46 | 54.4 | 12 |
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
| sqlite3MultiValues | insert.c | 61 | 75.4 | 32 | 53.1 | 12 |
| sqlite3VdbeSwap | vdbeaux.c | 19 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_stmt_status | vdbeapi.c | 15 | 40.0 | 4 | 75.0 | 1 |
| sqlite3_limit | main.c | 13 | 84.6 | 12 | 66.7 | 2 |
| sqlite3DecOrHexToI64 | util.c | 14 | 100.0 | 16 | 93.8 | 1 |
| sqlite3Atoi64 | util.c | 51 | 100.0 | 54 | 96.3 | 2 |
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
| getPageNormal | pager.c | 45 | 80.0 | 32 | 75.0 | 7 |
| sqlite3PagerUnrefPageOne | pager.c | 5 | 100.0 | 0 | 100.0 | 0 |
| pagerUnlockIfUnused | pager.c | 4 | 100.0 | 2 | 100.0 | 0 |
| pagerUnlockAndRollback | pager.c | 21 | 42.9 | 14 | 71.4 | 3 |
| sqlite3InitCallback | prepare.c | 57 | 79.0 | 56 | 58.9 | 19 |
| corruptSchema | prepare.c | 18 | 66.7 | 14 | 64.3 | 5 |
| sqlite3PagerCloseWal | pager.c | 19 | 63.2 | 18 | 27.8 | 6 |
| sqlite3BtreeGetFilename | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |
| sqlite3PagerFilename | pager.c | 4 | 100.0 | 6 | 66.7 | 1 |
| sqlite3ValueText | vdbemem.c | 7 | 100.0 | 8 | 100.0 | 0 |
| valueToText | vdbemem.c | 14 | 78.6 | 16 | 43.8 | 4 |
| sqlite3VdbeMemTranslate | utf.c | 64 | 79.7 | 74 | 85.1 | 8 |
| strHash | hash.c | 6 | 100.0 | 2 | 100.0 | 0 |
| insertElement | hash.c | 17 | 100.0 | 10 | 100.0 | 0 |
| rehash | hash.c | 16 | 100.0 | 8 | 87.5 | 1 |
| findElementWithHash | hash.c | 15 | 100.0 | 10 | 100.0 | 0 |
| sqlite3HashInsert | hash.c | 20 | 100.0 | 14 | 92.9 | 1 |
| findCollSeqEntry | callback.c | 20 | 85.0 | 8 | 75.0 | 2 |
| sqlite3FindCollSeq | callback.c | 6 | 100.0 | 4 | 100.0 | 0 |
| sqlite3LocateCollSeq | callback.c | 8 | 100.0 | 6 | 83.3 | 1 |
| createCollation | main.c | 28 | 89.3 | 24 | 70.8 | 5 |
| sqlite3GetCollSeq | callback.c | 14 | 100.0 | 14 | 100.0 | 0 |
| sqlite3ExprAnd | expr.c | 13 | 92.3 | 8 | 75.0 | 2 |
| sqlite3ExprSimplifiedAndOr | expr.c | 9 | 88.9 | 16 | 62.5 | 4 |
| sqlite3_compileoption_get | main.c | 5 | 100.0 | 4 | 100.0 | 0 |
| sqlite3_compileoption_used | main.c | 9 | 100.0 | 8 | 100.0 | 0 |
| sqlite3CompileOptions | ctime.c | 3 | 100.0 | 0 | 100.0 | 0 |
| compileoptiongetFunc | func.c | 4 | 100.0 | 0 | 100.0 | 0 |
| compileoptionusedFunc | func.c | 4 | 100.0 | 2 | 100.0 | 0 |
| sqlite3VdbeReusable | vdbeaux.c | 6 | 100.0 | 4 | 50.0 | 2 |
| sqlite3_value_int | vdbeapi.c | 2 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_value_text | vdbeapi.c | 2 | 100.0 | 0 | 100.0 | 0 |
| sqlite3VdbeIntValue | vdbemem.c | 9 | 100.0 | 8 | 87.5 | 1 |
| sqlite3RealToI64 | vdbemem.c | 4 | 100.0 | 4 | 100.0 | 0 |
| sqlite3IsIdChar | tokenize.c | 1 | 100.0 | 0 | 100.0 | 0 |
| sqlite3Strlen30 | util.c | 3 | 100.0 | 2 | 100.0 | 0 |
| fkLookupParent | fkey.c | 57 | 56.1 | 34 | 55.9 | 8 |
| fkScanChildren | fkey.c | 42 | 69.0 | 22 | 50.0 | 7 |
| fkActionTrigger | fkey.c | 98 | 95.9 | 57 | 77.2 | 13 |
| sqlite3FkCheck | fkey.c | 76 | 75.0 | 76 | 60.5 | 20 |
| sqlite3VdbeCheckFkDeferred | vdbeaux.c | 4 | 100.0 | 2 | 100.0 | 0 |
| sqlite3VdbeCheckFkImmediate | vdbeaux.c | 3 | 100.0 | 2 | 100.0 | 0 |
| sqlite3VdbeNextOpcode | vdbeaux.c | 45 | 55.6 | 32 | 53.1 | 10 |
| sqlite3VdbeCountChanges | vdbeaux.c | 3 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_get_autocommit | main.c | 5 | 100.0 | 0 | 100.0 | 0 |
| sqlite3RegisterLikeFunctions | func.c | 12 | 100.0 | 4 | 100.0 | 0 |
| likeFunc | func.c | 25 | 100.0 | 20 | 100.0 | 0 |
| patternCompare | func.c | 76 | 60.5 | 104 | 59.6 | 16 |
| isLikeOrGlob | whereexpr.c | 71 | 81.7 | 80 | 63.8 | 18 |
| sqlite3VdbeGetBoundValue | vdbeaux.c | 10 | 100.0 | 6 | 83.3 | 1 |
| sqlite3VdbeSetSql | vdbeaux.c | 7 | 100.0 | 4 | 100.0 | 0 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 206762 | 2/2 |
| parse.y:524 | 148938 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yedi adlandırılmış opcode ve Noop/Explain ortak default bloğu (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 16/20 |
| OP_Integer | 0/0 |
| OP_String8 | 6/10 |
| OP_ResultRow | 2/4 |
| OP_Expire | 2/2 |
| OP_Init | 10/20 |
| OP_Noop_OP_Explain_default | 0/0 |

## V6b.32 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeapi.c:1751–1768 | 6/7 | 6/12 |
| vdbeapi.c:170–181 | 8/8 | 3/4 |
| vdbeaux.c:146–153 | 5/5 | 0/0 |
| vdbeapi.c:432–434 | 2/3 | 1/2 |
| vdbeapi.c:666–668 | 2/3 | 1/2 |
| main.c:2047–2069 | 8/9 | 10/12 |
| vdbe.c:1852–1912 | 32/40 | 14/26 |
| vdbe.c:1617–1628 | 6/8 | 1/4 |
| vdbe.c:3328–3382 | 12/23 | 4/24 |
| vdbe.c:4074–4090 | 12/12 | 6/6 |
| vdbe.c:4186–4204 | 10/13 | 6/6 |
| vdbeaux.c:2997–3002 | 4/4 | 6/6 |
| vdbeaux.c:3554–3554 | 1/1 | 2/2 |
| main.c:2760–2763 | 2/2 | 2/2 |
| insert.c:1011–1011 | 1/1 | 2/2 |
| delete.c:421–421 | 1/1 | 2/2 |
| update.c:608–608 | 1/1 | 2/2 |
| vdbeaux.c:3613–3613 | 1/1 | 2/2 |
| vdbeapi.c:881–886 | 5/5 | 4/4 |
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeaux.c:3405–3516 | 46/63 | 42/60 |
| vdbeapi.c:990–1023 | 14/17 | 7/10 |
| vdbe.c:3136–3147 | 0/2 | 0/4 |
| vdbe.c:3210–3224 | 7/7 | 10/12 |
| vdbe.c:3393–3402 | 4/6 | 1/2 |

