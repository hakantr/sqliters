# Sohbet devri — V6b.33 sonrası devam

Bu dosya yeni sohbetin başlangıç belgesidir. Kullanıcının son talimatı, mevcut adım tamamlanınca çalışmayı başka sohbete devretmektir. Yeni model aşağıdaki durumu yerel Git ve sonuç dosyalarıyla kontrol ederek **V6b.34: mapped-page desteğinin sayfalayıcıya bağlanması** işinden devam etmelidir. Önceki adımları yeniden uygulamamalıdır. Yayın henüz hazır değildir; tam C eşliği veya bitiş tarihi garantisi verilmemiştir.

## Kullanıcının amacı ve çalışma kuralları

1. SQLite **3.54.1** ile aynı girdide veri, hata/uyarı iletileri ve veritabanı dosyasının bütün baytları aynı olmalıdır. Her fark kusurdur. C kaynakları `sqlite/` altında salt okunur referanstır. Referans source-id: `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`.
2. Öncelik eksikleri ve doğruluk farklarını gidermektir. Performans çalışması doğruluğu kanıtlanmış kod üzerinde yapılır. Bir adım bitince kullanıcıya sıradaki işi sorma; uygun işi kendin seçerek yayına kadar ilerle. Bu sohbetin sonunda durulmasının sebebi kullanıcının sohbet devri isteğidir.
3. Testin geçmesi tek başına kanıt değildir. C koşulu `dosya:satır`, sonucu ve Rust karşılığı karar tablosunda gösterilir. Gcov kapsamı ölçülür; alınmayan **her dal** ayrı sınıflandırılır. Alınmış bir C dalı otomatik olarak Rust eşliği sayılmaz. Kısmi ve kanıtlanmamış karşılıkları açık yaz.
4. Her adımda dört kapı: C ikilisi PATH'te çalışma alanı testleri `SQLITERS_VDBE_EXEC` kapalı/açık; DML farksalı; ayrışma bulucu; Rust **1.100** tabanına karşı hız ölçümü. Native API akışlarını, tam programları, elle yazılan SQL ifadelerini ve dosya baytlarını da işin kapsamına göre karşılaştır.
5. Doğrulanan her adımı commit ve push et; ayrıca izin sorma. `performans-A-vdbe` ve `performans-A-vdbe-makine` her adımda birbirine aktarılıp eşitlenir. Kirli kullanıcı değişikliklerini kaybetme. Ana çalışma alanındaki dalı değiştirme.
6. Türkçe iletişim ve raporlama. Raporlar `benchmarks/performans-A-vdbe/` altında `rapor.md`, `karar_esligi_*.md`, `gorulen_farklar_*.md`. Sonuçları temiz göstermek için eski hata beklentilerini değiştirme.
7. Aynı anda en fazla bir alt ajan; alt ajan açma yetkisi varsayma. Kullanıcı yeni alt ajan çalışmasını istemedikçe kendin çalış. Alt ajanlar alt ajan açmaz, commit etmez, `./sqlite` ve `rapor.md` dosyalarına dokunmaz. Çalışan ajan yarıda kesilmez. Önceki iki yayın sonrası belge ajanı işini bitirdi; devam işi yok.
8. `ileriPlanlama/` **yayından sonra** değerlendirilecek. Ürün geliştirme işinde dikkate alma. Bu dizin ayrı yerel Git deposudur; GitHub'a gönderilmez, ana depoya eklenmez. Önceki belge görevini yeniden açma.
9. Rust araç zinciri depoda **1.100.x** sabitlidir. `cargo +1.100 ...` kullan. LLVM/PGO için araç kurulumu kullanıcı onayı bekliyordu; onay alınmış varsayma.
10. Boyut küçültme en son: yalnızca davranışı değiştirmeyen kullanılmayan kod/artık temizliği. Davranış veya hız değiştiren ayarları otomatik uygulama; ölçüp kullanıcıya sun.

## Dallar ve kesin çalışma yolları

| Amaç | Yol | Dal |
|---|---|---|
| Ana checkout; değiştirme | `/home/hakan/github/sqliters` | `performans-A-hizalama` |
| Etkin geliştirme alanı | `/home/hakan/github/sqliters/.claude/worktrees/vdbe` | `performans-A-vdbe` |
| Makine dalı; her doğrulanan adımda eşitle | `/home/hakan/github/sqliters/.claude/worktrees/makine` | `performans-A-vdbe-makine` |
| Özel C/Rust düzenekleri ve sonuçlar | `/home/hakan/github/sqliters/target/perf-work` | Git dışı yerel kanıtlar |
| C 3.54.1 CLI | `/home/hakan/github/sqliters/target/perf-work/bin/sqlite3` | Referans ikili |

Uzak depo: `git@github.com:hakantr/sqliters.git`. Yeni worktree veya yeni dal gerekmez. Zed'de zaten kullanımda olan dalı başka checkout'a almaya çalışmak yerine mevcut `.../.claude/worktrees/vdbe` dizinini aç.

Bu belgeyi taşıyan commit V6b.33 kayıt noktasıdır. Önceki doğrulanan kayıtlar:

- V6b.31: `bce97bebd54559a75a1903aa42cd816a98385f93`.
- V6b.32: `43d1799780ea282d695a26c279a90adccdf0691d` — iki dala da push edildi.
- V6b.33 commit kimliği yerelde `target/perf-work/v6b33/res/verified_commit.txt` içinde tutulur; iki dal ve origin uçları karşılaştırılmalıdır.

İlk kontrol:

```bash
git -C /home/hakan/github/sqliters/.claude/worktrees/vdbe status --short --branch
git -C /home/hakan/github/sqliters/.claude/worktrees/vdbe log -3 --oneline
git -C /home/hakan/github/sqliters/.claude/worktrees/vdbe rev-parse HEAD origin/performans-A-vdbe
git -C /home/hakan/github/sqliters/.claude/worktrees/makine rev-parse HEAD origin/performans-A-vdbe-makine
cat /home/hakan/github/sqliters/target/perf-work/v6b33/res/validation.txt
```

## Son tamamlanan işler

Kullanıcının ilk istediği `default_cache_size` hazırlama etkisi V6b.3, `synchronous` V6b.4, `page_size` V6b.5, `auto_vacuum` V6b.6 olarak işlendi ve raporlandı. V6b.7–V6b.28 arasında bağlantı/cache, locking/temp_store/encoding hazırlaması, şema başlatma, threads, shrink_memory, kısa okuma ve page1, database_list, collation_list, compile_options, ertelenmiş FK, EXPLAIN yazıcı ömrü ve callback/savepoint işleri yapıldı. Bunları yeniden başlatma; dar kapsamları ve devam eden açıkları her adımın `karar_esligi_v6b_*.md` dosyasından doğrula. Buradaki tamamlandı ifadesi ilgili kanıt kapsamınadır; tüm PRAGMA/M4 veya tüm API eşliği değildir.

### V6b.31 — UTF-16 / LIKE / NOCASE

Ham token baytları, C'nin UTF-8/UTF-16 dönüştürmesi, BINARY veritabanı kodlaması, NOCASE/RTRIM UTF-8 karşılaştırmaları, NUL ve bozuk metin yolları eşitlendi. Numeric UTF-16 prefix/rc=-100, CAST, bind ve MakeRecord kodlamaları düzeltildi. 489.145 ham dönüşüm ve 1.587.945 sayı dönüşümü C ile aynı; diğer native/tam program matrisleri raporda. **UTF-16 makine okuma kabulü hâlâ yalnız `references_like` için**; tüm sorgulara genişletilmiş sayılmaz.

### V6b.32 — mmap hazırlama etkisi ve TEMP dosyası

`PRAGMA mmap_size` kod üreticide typ28; `PrepareEffect::MmapSize`, bağlantı varsayılanı, açık Btree'lere ters slot sırasıyla aktarım, dosya denetimi, NOTFOUND halinde metadata olup sonuç satırı olmaması, hazırlama anındaki snapshot ve EXPLAIN etkisi taşındı. Her iki anahtarda `prepare_row_pragma` kullanılıyor; constant integer bridge'e ekleme.

Önemli TEMP ayrıntısı: **Btree yeni açılırken bağlantının mmap varsayılanını devralır** (`btree.c:2713–2716`); fiziksel dosya daha sonra ilk spill'de açıldığında fd limiti yine **0** başlar. `pagerOpentemp` bekleyen limiti o anda uygulamaz. Journal rollback / transaction-savepoint geometry restorasyonu daha sonra hint'i yenileyebilir. Bu mirası silme ve fiziksel açılışa hint ekleme.

TEMP küçük commit'leri kirli sayfaları tutar; dosya `begin_write` sırasında değil ilk gerçek sayfa yazımında açılır. PCache yüzde25 eşiği ve writable temizliği taşındı. 270 TEMP akışı aynı, 144 tam mmap programı aynı. 250 native mmap akışının 240'ı aynı; dört D144 DDL offset ve altı D142 etkin okuyucu limiti farkı açık. Kalıcı ana/aux dosyalarının bütün hash'leri aynı. Anonim TEMP fiziksel dosya baytlarının dökümü ayrıca kanıtlanmadı.

### V6b.33 — VFS fetch/unfetch altyapısı

OFF (1131, 4, 337); ON (1117, 18, 337); fail kümeleri V6b.32 ile aynı, üç yeni VFS testi geçti.

DML 378/400 OFF,398/400 ON; bütün sınıf/seed JSON'u aynı.

Finder245 stdout245 stderr244; önceki raporla tam aynı.

Fetch10051/model/Rust exact; fixture bütün bayt hashleri aynı.

Native mmap240/250 (D142 altı,D144 dört açık); TEMP270 ve tam program144 exact.

Elle CLI16 exact; gcov46dal,15never sınıflı.

Rust1.100 tabanına hız 1.210x;14digest0 fark.

Dört kapının eski başarısızlık kümeleri korundu; tüm testler yeşil değildir. Detaylar `karar_esligi_v6b_33.md` ve `karar_esligi_v6b_33_dallar.md`.

Üretim değişiklikleri:

- `crates/sqliters-vfs/src/lib.rs`: `FetchPage`, VfsFile `fetch` / `unfetch_all` varsayılanları, Unix OsFile'ın paylaşılan fetch durumu; kapatmadan önce haritayı bırakma.
- `crates/sqliters-vfs/src/fetch.rs`: mmap limiti, harita ve outstanding fetch sayısı. Negatif limit sorgudur; üst sınır `0x7fff0000`. Aktif fetch varken limit güncellemesi reddedilir. Son RAII lease bırakıldığında güncelleme tekrar kabul edilir. `unfetch_all` mevcut haritayı kaldırır. 256 bayt dosya sonu payı korunur.
- `crates/sqliters-vfs/src/sys/unix.rs`: mevcut izinli Unix FFI sınırı içinde gerçek salt okunur `MAP_SHARED mmap/munmap`; ham pointer üst katmana çıkmaz. `ReadMapping` veriyi atomik bayt okumalarıyla sahipli Vec'e kopyalar. Send güvenlik gerekçesi ve Drop sahipliği kaynakta yazılı.
- `crates/sqliters-vfs/tests/fetch.rs`: üç yaşam süresi, EOF/invalidation ve remap/yeni veri testi.

**Sayfalayıcı VfsFile::fetch'i henüz çağırmıyor. D142 kapanmadı.** V6b.33 bir VFS altyapı adımıdır. Kernel haritasından sahipli sayfaya kopya yapılır; sıfır kopya veya hız kazanımı iddiası yoktur. C mmap tahsis hatasındaki `sqlite3_log` uyarısı, mevcut haritayı `mremap` ile uzatma, tam hata enjeksiyonu ve Windows fetch yolu tamamlanmış/kanıtlanmış sayılmaz. Başarılı Unix xFetch kapsamını genelleme.

## Sıradaki somut iş — V6b.34 / D142

1. `crates/sqliters-pager/src/lib.rs` içinde C `getPageMMap` (`pager.c:5701–5769`), `pagerAcquireMapPage` (`4110–4162`), `pagerReleaseMapPage` (`4167–4177`), unref (`5849...`), `pagerFixMaplimit` (`3587...`) ile sayfa getter/ref yaşamını birlikte oku.
2. Mapped sayfalar normal PCache'den ayrıdır; cache hit/miss, dirty/LRU ve `sqlite3PagerRefcount` içinde sayılmaz. C her acquire için ayrı `PgHdr` ve `nRef=1` alır. Page1 hiçbir zaman mapped değildir. Page1/read-lock bırakılmadan mapped ref'ler bırakılır.
3. `pgno>1 && (PAGER_READER || PAGER_GET_READONLY)` guardını, WAL frame varsa normal yolun seçilmesini ve hata sırasını taşı. TEMP veya write/READONLY sırasında **önce xFetch**, sonra normal cache lookup yapılır; normal disk read'de C, sayfa normal cache'de olsa bile xFetch'i kullanır. Sadece cache'e yeni sayaç koymak yeterli değildir.
4. Rust API'leri `Pgno` döndürür; mapped `PgHdr` kimliği yok: `page_ref`, `page_ref_btree`, `page_ref_nocontent`, `page_lookup`, `page_refcount`, `page_unref`, `read_page`, `read_page_unchecked`. İç `pager_get` normal cache slotu döndürür ve yazım yollarında kullanılır. Bunu rastgele mapped slot gibi kullanma. Ayrı mapped kayıt/ref/lease yapısı veya açık bir sayfa-kaynağı türü tasarla. Aynı pgno'ya birden fazla acquire/unref ve normal+mapped ref'in karışması özellikle incelenmeli.
5. `transient_pin`, `release_transient`, `pcache_reset`, page-size değişimi, `set_state(Open)`, `release_all_refs`, rollback/savepoint ve cache epoch yaşamını kapsa. Slice döndüren yardımcı okumalar hayalet uzun ömürlü fetch tutmamalı. `cell_size_check` C'nin yeni mapped PgHdr doğrulamasını atlamamalı.
6. Pager'ın bekleyen `szMmap` isteği, getter'ın `bUseFetch` durumu ve fd'nin kabul ettiği `mmapSizeMax` farklıdır. Aktif okuyucu yüzünden dosya isteği reddetse de pager isteği değişebilir. Yeni TEMP fd'sine bekleyen isteği erken uygulama. `predict_mmap_size` artık outstanding guardını hesaba katmak zorunda kalabilir.
7. `Connection::Statement::c_machine_of` (`crates/sqliters-core/src/lib.rs`, `fn c_machine_of`) OFF iken yalnız LIKE için C makinesine zorlar. Eski yürütücü kimi okumaları materialize/sort edip farklı leaf cursor tutabilir. Bu yüzden sadece ON altı örneği düzeltmek yetmez: OFF yolu, son sayfanın 256-byte EOF nedeniyle mapped olmaması, dar mmap aralığı, ORDER BY ve cursor finalize/reset senaryolarını ayrı sınamalısın. Tüm okumaları kanıtsız makineye zorlayıp diğer eksikleri açığa çıkarma; UTF-16 kabul guardı ve bound LIKE/GLOB hazırlama yolları da hesaba katılmalı.
8. Önce mevcut `v6b33/mmap_initial.py` içindeki `active_map` örneklerini hem OFF hem ON C ile karşılaştır. Altı D142 farkını ve dosya hash'lerini incele; ardından çoklu cursor, son sayfa/EOF, limit sınırı, cache shrink, reset/finalize, WAL frame, TEMP, write-READONLY, rollback, attached veritabanı ve hata akışlarına genişlet.
9. D142 kapanırsa `mmap_known.py` beklentisini altı D142 artık aynı, dört eski D144 açık olacak şekilde **sadece kanıtla** güncelle. Farkları normalize ederek geçen sayma. Karar tablosu, kapsam, hiç alınmayan dallar ve dört kapıdan sonra commit/push; makine dalını ff ile eşitle.

## Kanıt düzenekleri ve yeniden çalıştırma

`/home/hakan/github/sqliters/target/perf-work/v6b33/`:

- `fetch_probe.c`, `fetch-c`: gerçek C VFS FILE_POINTER üstünden xFetch/xUnfetch/MMAP_SIZE. C önce 417.792 bayt fixture oluşturur.
- `fetch_model.rs`, `fetch-model`: önceki bağımsız güvenli durum modeli; üretim kodu değildir.
- `fetch-r/`, `fetch-r-bin`: gerçek yeni Rust VFS'sini çağıran driver.
- `fetch_matrix_product.py`: sabit seed354133; C/model/Rust 10.051 geçerli işlem. `res/fetch_inputs.txt`, `fetch_C.txt`, `fetch_model.txt`, `fetch_Rust.txt`, `fetch_bugs.json`, `fetch_summary.json`.
- `cov/`: özel C amalgamation/object/gcno/gcda; özgün `sqlite/` değiştirilmez. `gcov -f -b -c sqlite3.c -o .` bu dizinde çalıştırılır. `fetch_proof.py` C amalgam satırlarını özgün `sqlite/src/os_unix.c` satırlarına eşler. 46 dalın 15'i alınmadı; hepsi raporda sınıflı. Bu profil önceki büyük ana profil ile birleştirilmiş kapsama sayılmaz.
- `check.sh`: release CLI/speedbench derlemesi, C PATH'te OFF/ON tüm workspace testleri.
- `gates.sh /mutlak/yol/sqliters last`: DML seed6001/400 OFF ve ON, finder245. Girdi/çıktı dosyaları `res/dmlfuzz_last_*.json/txt`, `finder_last.txt`, `finder_last/`.
- `mmap_initial.py`, `temp_matrix.py`, `mmap_full_programs.py`, `mmap_known.py`: önceki native250, TEMP270, tam program144 matrisleri. `bin/like-r` yeni derlenmeli; C `bin/like-v1-driver`, `bin/like-full-driver` aynı referans object'tendir.
- Hız: `python3 /home/hakan/github/sqliters/target/perf-work/ab.py --trials 7 --base-dir /home/hakan/github/sqliters/target/perf-work/v4_bin/base_1100 --cand-dir /home/hakan/github/sqliters/target/perf-work/v6b33/bin`. Diğer işler bittikten sonra çalıştır; 14 digest de kontrol edilir.
- `res/source_manifest.json`, `validation.txt`, `oracle_and_binaries.json`, `verified_commit.txt`: doğrulanan kaynak, sonuç ve kayıt noktası. Bunlar yerel ve Git dışı; aynı hosttaki yeni sohbet okuyabilir. Kalıcı Türkçe özet/karar raporları Git'tedir.

P32 (`.../v6b32/`) büyük önceki kanıt takımını içerir: 13.529 aynı program, 16.027 EXPLAIN, 16.027 EQP, 11.789 yürütme, native API ve I/O, codec/numeric/LIKE matrisleri. Yeni adıma körlemesine geçer diye kopyalama; değişen kapsama göre yeniden çalıştır. `res_before_inheritance` eski kaynak ölçümüdür; güncel P32 sonucu olarak kullanma.

Düzenek dersleri:

- Driver derlemesi bitmeden binary'yi çalıştırma. V6b.33'te erken `like-r` yok denemesi `res/early_missing_driver_*` altında arşivlendi; son tekrar geçerli sonuçtur.
- `mmap_full_programs.py` için `WRITER_TAG=mmap_full_programs`; yanlışlıkla `mmap_final` yazıp 250 akış JSON'unu ezme.
- Ops girdisi `ops.rstrip('\n')+'\n'`; C özel driver boş satırda eski parçayı kullanabiliyordu. Bu SQLite kusuru değil düzenek hatasıydı.
- Mutlak `/.../bin/sqliters` kullan; finder temp cwd içinde göreli yol çalışmaz.
- Yeni step kopyalanırken hardcoded `v6bXX` yollarını incele. Mutlak yolda `/target/` geçtiği için kaynakları topluca dışlayan filtre yazma; göreli path parçalarını filtrele.
- Eski C object'ine yeni gcda checksum'ı karıştırma. Yeni özel probe derlemelerine ayrı ad/gcov prefix ver. Numaralandırılmış çalışmaları ve source manifest'i kontrol et; yalnız `.done` dosyasına güvenme.
- Uzun genel dosyalara otomatik rustfmt uygulama; kapsam dışı değişiklik yaratma. `git diff --check` ile bitir.

## Mevcut açık farklar ve sonraki sıra

- **D142:** etkin mapped-reader sırasında mmap limitinin korunması. VFS altyapısı var, pager bağlantısı yok.
- **D143 / D57 module_list:** C native başlangıçta `dbstat, fts5, fts5vocab, rtree, rtree_i32`; Rust eager sıralı10 modül. CLI C'de generate_series/fsdir/completion da var. C'de pragma TVF/JSON modülleri tembel kaydediliyor; hash ekleme/silme geçmişi sıra için önem taşıyor. Var olmayan hizmet için yalnız modül adı kaydıyla eşlik taklidi yapma. CLI fsdir/completion da eksik.
- **D144:** zaten var olan `CREATE TABLE t` C error offset13 / Rust-1; mmap matrisindeki dört warm/reopen olayı. DDL hazırlama/M4 alanı.
- **D52:** 1.644 legacy RETURNING akışının20'sinde sayaç farkı; dosya baytları aynı. **D103:** 4.158 akıştan bir OFF locking_mode TVF ek exclusive NULL satırı. Eski kusurlar açık.
- **E27 / E28:** anonim TEMP ham dosya kanıtı ve genel I/O yolları; COMMIT yazım I/O hatasında C rollback, Rust işlemi açık tutuyor. Pager ERROR durumu eklenmeden E28 düzeldi denmez.
- **F4, F6–F9** C dışı eski test beklentileri. V6b.32'de **F10** memory mmap tek0satırı, **F11** private TEMP xOpen begin_write, **F12** TEMP lock_status uzantısı eski beklentileri eklendi. Legacy kaldırılana kadar beklentileri değiştirme. Hatalı testler yeşil sayılmaz. C hedefinde SQL PRAGMA lock_status DEBUG dışında yok; F12'nin C kanıtı FILE_CONTROL LOCKSTATE=NOTFOUND üzerinden, Rust `'unknown'` metni C SQL eşliği değildir.
- V6b'nin kalan PRAGMA'ları: function_list/module_list, mmap runtime, temp_store_directory, soft/hard_heap_limit, integrity_check, optimize ve envanterde açık diğerleri.
- **M4:** DDL'nin makinede yürütülmesi: SetCookie, ParseSchema, CreateBtree, Expire, Drop*, Destroy, SqlExec, Vacuum; işlev kitaplığında sqlite_rename_*, stat_*, sqlite_attach/sqlite_detach. V6b altında Expire'ın görülmesi global M4 bitişi anlamına gelmez.
- Veri/semantik: D23 (`0.5 IS TRUE`), D49 metinden sayıya, D50 bind uzunluk sınırı, D55 query_only, D56/D57 pragma TVF, D59–D74/D81 DDL, JSON5 Unicode; her birinin son dal raporundan güncel durumu doğrulanır.
- Sonra eski yürütücü/hızlı yollar kaldırılır; C dışı test beklentileri C'ye çevrilir; kalan hızlı yollar denetlenir.
- `performans-A-hizalama` ve VDBE sonuçları `performans-A`'ya aktarılır. Birleştirme sonrası doğruluk yeniden doğrulanır, sonra hız geri kazanılır.
- `performans-A` C eşliği listesi: tarih/saat, PRAGMA optimize, vtab changes/total_changes, UTF-16/geçersiz UTF-8, integrity_check artıkları, eksik nokta komutları/kabuk işlevleri, etkileşimli CLI.
- Ardından M3b ve `-morsel`, `-io`, `-pgo`, `-simd`, `-hibrit` performans işleri; PGO kurulum izni hâlâ ayrı konu. Son tüm testler/resmi hız raporu/yayın kontrolü, en son davranış değiştirmeyen boyut temizliği.

## Yeni sohbete verilecek başlangıç talimatı

> `/home/hakan/github/sqliters/.claude/worktrees/vdbe/benchmarks/performans-A-vdbe/devir_v6b_33.md` dosyasını oku. Belgedeki Git uçları ve doğrulama kayıtlarını kontrol et; çalışma alanı `/home/hakan/github/sqliters/.claude/worktrees/vdbe`, dal `performans-A-vdbe`. V6b.33'ü tekrar yapmadan V6b.34/D142 mapped-page desteğinin sayfalayıcıya bağlanmasından devam et. C SQLite 3.54.1 eşliği, karar tabloları/kapsam, dört doğrulama kapısı, commit/push ve makine dalıyla eşitleme kurallarını uygula. `ileriPlanlama`yı kapsam dışında tut. Doğruluk önce; sonraki uygun adımı kendin seçerek yayın aşamasına kadar ilerle.
