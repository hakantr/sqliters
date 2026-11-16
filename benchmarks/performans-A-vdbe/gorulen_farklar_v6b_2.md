# V6b.2 — önbellek, günlük sınırı ve güvenli silme farkları

Başlangıç **84daa83**. Üç yeni PRAGMA ailesi: `cache_size`, `journal_size_limit`, `secure_delete`. Karar ve ölçüm kaydı [karar_esligi_v6b_2.md](karar_esligi_v6b_2.md); her alınmayan dalın ayrı sınıfı [karar_esligi_v6b_2_dallar.md](karar_esligi_v6b_2_dallar.md). Yerel sonuçlar `/home/hakan/github/sqliters/target/perf-work/v6b2/` altında.

## Düzeltilen farklar

**D71'in bu üç ailedeki hazırlama kısmı kapandı:** ayar normal PRAGMA ve EXPLAIN hazırlamasında uygulanıyor. Sorgu ve ayarın sonuç değeri hazırlanmış programda sabit; daha sonraki ayar ilk ifadenin sonucunu değiştirmiyor. `secure_delete=FAST` niteliksizse bütün açık Btree'lere, nitelikliyse yalnız hedefe uygulanıyor. Bu üç işleyici diğer ifadeleri geçersizleştiren OP_Expire üretmiyor. Genel PRAGMA makine geçişi yapılmadı.

**D83 — cache_size sayısal dönüşümü ve writable_schema=RESET sonrası sıfır değerin yeniden yüklenmesi (düzeltildi).** C `sqlite3Atoi` taşmada 0 döndürür; eski yürütücü i32 sınırına sıkıştırıyordu. `cache_size=0` etkin şemada sıfır kalır. `writable_schema=RESET` ardından şema yeniden yüklenirse C `sqlite3InitOne` sıfır cache_size değerini dosya çerezinden, o da sıfırsa -2000 ile başlatır. Rust'ın “önceden ayarlandı” bayrağı bu ikinci başlatmayı engelliyordu. Sıfır değerin yeniden başlatılmasına yalnız C'nin gerçek RESET noktasında izin veriliyor; Rust'ın her DDL sonrası yaptığı iç şema yüklemesi bu noktadan ayrılıyor.

| Girdi / durum | C | V6b.1 | V6b.2 |
|---|---|---|---|
| cache_size=0; cache_size | 0 | 0 | 0 |
| Ardından writable_schema=RESET; cache_size | -2000 | 0 | -2000 |
| cache_size=2147483648; cache_size | 0 | 2147483647 | 0 |
| temp.journal_size_limit=29; temp.journal_size_limit | 29, ardından 29 | -1, ardından -1 | 29, ardından 29 |

Son satır eski yürütücünün TEMP'e koyduğu sabit yanıtın doğrudan PRAGMA yolundaki etkisidir; bu dilimin sabit sonuç köprüsü C'nin gerçek pager değerini kullanır. Tablo değerli pragma işlevlerinin D57 farkları ayrıca açık; bunların tamamının düzeldiği ileri sürülmüyor.

**D84 — PRAGMA içinde büyük hex token (düzeltildi).** `PRAGMA journal_size_limit=0x10000000000000001` C'de 1 döndürür; önceki Rust ön ayrıştırıcısı `hex literal too big` hatasıyla üreticiye ulaşmadan reddediyordu. C `parse.y:1732–1741` sayısal PRAGMA değerini ham token olarak geçirir. Bu yolda ifade sabiti dönüşümü başarısızsa ham metin korunuyor; negatif i64 sınırında da çarpma taşması oluşmuyor. Normal SQL ifadelerinin büyük hex denetimi değişmedi. Kanıt `res/journal_int64_before.json`, `res/journal_int64_runtime.json`.

**D85 — sabit PRAGMA köprüsünde C derleme hatası (düzeltildi).** `PRAGMA journal_size_limit=-'17'` C'de sözdizimi hatasıdır; eski ön ayrıştırıcı bunu kabul ediyordu. Üreticinin Error sonucu köprüde eski yürütücüye bırakılınca Rust 17 döndürüyordu. Köprü şimdi C'nin ileti ve byte konumunu hem hazırlama hem yeniden hazırlamada döndürüyor; yalnız Unsupported eski yola bırakılıyor. Aynı sınama cache_size ve secure_delete için de iki anahtar durumunda CLI stdout/stderr/çıkış kodu ve caret konumuyla karşılaştırıldı (`res/pragma_error_before.json`, `res/pragma_error_after.json`).

Kanıt: `res/legacy_before.json`, `res/cache_reset_before.json`, `res/legacy_after.json`. Üç yeni regresyon testi eklendi; mevcut C karşılaştırmalı test 60 yeni ifade ile 330'a çıktı. C'nin oluşturduğu dosyadaki default_cache_size=83 değeri yalnız hazırlamada okunuyor, sonraki 89 ayarına rağmen ilk sonuç 83 kalıyor. Gerçek hazırlama/adımlama dizileri ve yalnız hazırlanmış secure_delete=ON sonrası silmenin **dosya baytları** C ile aynı (`res/manual_api.json`, `res/manual_final.txt`). WAL/cache_size=0/CREATE TABLE senaryosu da iki kipte dosya baytlarıyla karşılaştırıldı.

Mevcut H6 dosya eşliği testi, sıfır önbelleğin sıradan CREATE TABLE ardından yeniden başlatılmasını reddeder. Ara düzeltmede bu test düştü (`res/tests_final_off.txt`); son uygulama sıfırı DDL boyunca korur, RESET ardından yeniden başlatır. Beklenen C dosya özeti değiştirilmedi (`res/page_one_fixed.txt`).

## Kapsam ve açık işler

Genişletilmiş derlemde kabul edilen **4.178 EXPLAIN / EQP** çıktısı ve **4.176 tam program** C ile aynı. 264 EXPLAIN girdisi ve 246 tam program girdisi desteklenmiyor; eski yola düşen çıktılar kanıta eklenmedi. Ardışık derlemde 179 kabul edilen ifade aynı; gizli görünüm geçmişi için bir girdi desteklenmiyor.

pragma.c kapsamı 495/990 satır (%50,0), 356/706 dal (%50,4). Eklenen üç Btree/pager yardımcısında 20/20 satır, 6/6 dal; toplam 515/1010 satır, 362/712 dal. Taşınmayan işleyiciler paydada; oran tamamlanma yüzdesi değildir. Hiç alınmayan 350 dal ayrı ayrı sınıflandırıldı. Bellek/GÇ enjeksiyonu, bozuk dosya, özel VFS ve yetkilendirici yolları ayrıca kanıt bekliyor.

Henüz taşınmayan 21 pragma: `auto_vacuum`, `busy_timeout`, `cache_spill`, `collation_list`, `compile_options`, `database_list`, `encoding`, `function_list`, `hard_heap_limit`, `integrity_check`, `locking_mode`, `mmap_size`, `module_list`, `quick_check`, `shrink_memory`, `soft_heap_limit`, `synchronous`, `temp_store`, `temp_store_directory`, `threads`, `wal_autocheckpoint`. Kısmi ailelerde page_size/case_sensitive_like ayarları, defer_foreign_keys işlem yaşam döngüsü, optimize kullanım geçmişi ve D82 görünüm geçmişi açık.

**D71'de ayrıca açık:** `default_cache_size=17` yalnız hazırlanır, adımlanmadan `cache_size` sorgulanırsa C 17, Rust -2000 döndürür. Bu fark V6b.1'de de var (`res/default_cache_pending_open.json`); bu üç aile dışındaki hazırlama etkileri tamamlanmadı. Yeni kabul sayıları bu bağlantı geçmişinin eşliğini kanıtlamaz.

**Sonraki adım:** önce `default_cache_size` hazırlama etkisini kapatmak; ardından `synchronous`, `page_size` ve `auto_vacuum` hazırlama durumunu taşımak. Özellikle C'nin bağlantı düzeyindeki sonraki sayfa boyutu/auto-vacuum durumu ile pager'ın mevcut değeri ayrı izlenecek; davranış kanıtlanmadan hız ayarı yapılmayacak. ileriPlanlama/ yayın sonrasına bırakıldı.

## Dört kapı

Çalışma alanı testleri: kapalı **1040 geçti / 0 başarısız / 337 yok sayıldı**; açık **1025 geçti / 15 bilinen başarısız / 337 yok sayıldı**. Açık kipteki başarısız test adları V6b.1 ile birebir aynı; beklentiler değiştirilmedi. Son kaynak sürümünün kayıtları `res/tests_verified_off.txt` ve `res/tests_verified_on.txt`.

DML kapalı 378/400 ve açık 398/400; bütün başarısız seed/sınıf kümeleri V6b.1 ile aynı. Ayrışma bulucu 245 senaryoda 241 L0, 2 L1, 1 L4, 1 L6; bilinen g06 stderr farkı sürüyor. Rastgele rowid senaryosunun sayıları ile ikili yolu normalleştirilerek önceki sonuç dosyaları ayrıca karşılaştırıldı. Hata kümelerinin aynı olması kalan farkları kapatmaz.

Hız kapısı (CPU6, tmpfs, 7 A/B denemesi; Rust1.100 tabanı): toplam 298,70→297,82ms (**0,997x**), geomean 0,991x, 14 sonuç özeti aynı. W06 tabana göre 1,020x, W07 ise 1,021x; bütün iş yükleri gerilemesiz sayılmadı. W06 ayrıca önceki V6b.1 ikilisine karşı 15 denemede 11,08→11,02ms (**0,994x**). V6b.1 aşamasında kaydedilen önceki V6'ya göre W06 borcunun kapandığı bu karşılaştırmadan çıkarılmıyor. Hız ayarı uygulanmadı; sonraki iş doğruluk eksikleridir.
