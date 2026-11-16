# V6b.1 — PRAGMA aktarımında bulunan farklar ve açık işler

Başlangıç `e9ba0e2`; C SQLite 3.54.1, Rust 1.100.0. Kanıt ve karar tablosu [karar_esligi_v6b_1.md](karar_esligi_v6b_1.md), alınmayan 369 dalın ayrı sınıfları [karar_esligi_v6b_1_dallar.md](karar_esligi_v6b_1_dallar.md). Yerel sonuç kökü `/home/hakan/github/sqliters/target/perf-work/v6b/`.

**V6b açık.** Kabul edilen 3.695 EXPLAIN ve 3.667 tam programda fark çıkmaması bütün PRAGMA davranışının taşındığı anlamına gelmez. 297 EXPLAIN girdisi açıkça desteklenmiyor; bunların eski yoldan gelen çıktısı üretici kanıtına katılmadı. Genel PRAGMA yürütmesi hâlâ eski yürütücüde; bu dilimde bayraklar ve `analysis_limit` için hazırlama etkileri ile sabit sonuçlar hizalandı.

## Bu dilimde düzeltilenler

- **D71 kısmen kapandı:** bayrak ayarları ve `analysis_limit`, EXPLAIN dahil hazırlamada uygulanıyor. Sorgu sonucu hazırlamadaki değerini koruyor. Ayar ifadesini daha sonra çalıştırmak önceki değeri yeniden uygulamıyor. Diğer hazırlanmış ifadeler C'nin `OP_Expire` zamanında, yani ayarın çalıştırılmasında geçersizleşiyor. İşlem içindeki `foreign_keys` değişimi etkisiz. `encoding`, `page_size` ve diğer pager/bağlantı hazırlama etkileri bekliyor; D71 bütünüyle kapanmadı.
- **Bağlantı maskeleri:** `trusted_schema` iki kez terslenmiyor. API ile değişen FK, tetikleyici, count_changes ve writable_schema bayrakları mutlak bağlantı değerinden yeniden kuruluyor. `SQLITE_DBCONFIG_WRITABLE_SCHEMA`, C `main.c:1014–1015` gibi `WriteSchema` ve `NoSchemaError` bitlerini birlikte temizliyor. Defensive durumunda ham ayar ile etkin yazma izni ayrılıyor.
- **Salt okunur inceleme:** `c_generated_program` hazırlama etkilerini bırakmıyor; aksi halde aynı iş parçacığında başka bağlantının yeniden hazırlanmasına sızıyordu. `res/isolation_before.txt` ve `res/dbconfig_mask_before.txt` düzeltmelerden önce düşen regresyon sınamalarıdır.
- **Yeni üreticide doğrulama sırasında bulunan kusurlar:** FK anahtarının affinity metni yalnız FK sütunu sayısı kadar kesiliyor; tablo karma sırası CHash kimliklerinden alınıyor; açılmamış `temp` üzerinde EXPLAIN, pragma adı aramasından önce C'nin `SQL logic error` hatasını veriyor; `wal_checkpoint(NOOP)` işlem kodunun türü C gibi **-1**. Bunlar son karşılaştırmalar alınmadan düzeltildi.

`wal_checkpoint(NOOP)` eski yürütücüsünde ayrıca bir kusur bulunmadı. Gerçek WAL dosyasında art arda iki NOOP, C ve Rust'ta `0|3|0` verdi (`res/noop_runtime.txt`). C'nin oluşturduğu 1024 bayt sayfalı dosya yeniden açıldığında page_size, table_info, index_xinfo, foreign_key_list ve table_list programları da karşılaştırıldı (`res/page_size_reopen.txt`, `res/reopen_metadata.txt`).

## D82 — başka şemadaki aynı adın gizlediği görünümün table_list geçmişi

```sql
ATTACH ':memory:' AS aux;
CREATE TABLE t(a);
CREATE VIEW aux.t AS SELECT 1 AS x, 2 AS y;
PRAGMA aux.table_list('t');
SELECT * FROM aux.t;
PRAGMA aux.table_list('t');
```

| Adım | C | Rust eski yürütücü |
|---|---|---|
| İlk table_list | `aux\|t\|view\|0\|0\|0` | `aux\|t\|view\|2\|0\|0` |
| SELECT | `1\|2` | `1\|2` |
| Son table_list | `aux\|t\|view\|2\|0\|0` | `aux\|t\|view\|2\|0\|0` |

C `pragma.c:1292–1320` görünüm sütunlarını başlatmak için şemasız bir dummy SELECT hazırlar. `main.t`, `aux.t` görünümünü gizlediği için ilk sorguda görünümün nCol değeri sıfır kalır. Görünüm daha önce kullanılmışsa C sakladığı iki sütunu döndürür. Mevcut şema bağdaştırıcısı bu kullanım geçmişini taşımıyor. Üretici bu bağlamda açık `Unsupported` verir; eski yürütücünün ilk sonucu hâlâ kusurludur. Kanıt `res/hidden_view_runtime.txt`; yeni test yaklaşık bir programın kabul edilmesini engeller. **D82 kapanmadı.**

## Kalan üretici işleri ve sınırlar

Henüz taşınmayan 24 pragma (23 işleyici türü): `auto_vacuum`, `busy_timeout`, `cache_size`, `cache_spill`, `collation_list`, `compile_options`, `database_list`, `encoding`, `function_list`, `hard_heap_limit`, `integrity_check`, `journal_size_limit`, `locking_mode`, `mmap_size`, `module_list`, `quick_check`, `secure_delete`, `shrink_memory`, `soft_heap_limit`, `synchronous`, `temp_store`, `temp_store_directory`, `threads`, `wal_autocheckpoint`.

Kısmi ailelerde `page_size` ve `case_sensitive_like` ayarları açık. **D72** (`defer_foreign_keys` işlem yaşam döngüsü) açık `Unsupported`; eski yürütücü davranışı düzeltilmedi. `optimize` istatistik kullanım geçmişi/TF_MaybeReanalyze gerektirdiğinde açık `Unsupported`. Yetkilendirici, özel VFS cevapları, bellek/GÇ hata enjeksiyonu, bozuk dosya ve alınmayan sınır dalları ayrıca kanıt bekliyor. PRAGMA programlarını genel olarak makinede çalıştırma bu dilimde yapılmadı; M4 işlem kodları ve işlev kitaplığı da açık.

Sonraki doğruluk adımı, pager ve bağlantı durumunu okuyup hazırlamada değiştiren pragmaların sözleşmesini taşımak; ardından saklanan şema/görünüm ve ertelenmiş FK geçmişini tamamlamak. Performans ayarı bu eksikleri kapatmanın önüne geçmeyecek. `ileriPlanlama/` dosyaları yayın sonrasına bırakıldı.

## Dört kapının sonucu

| Kapı | Sonuç | Önceki V6 ile karşılaştırma |
|---|---|---|
| DML farksalı, 400 seed | kapalı 378/400; açık 398/400 | Başarısız seed ve hata sınıfı kümeleri aynı; `res/gate_comparison.json` |
| Ayrışma bulucu, 245 senaryo | 241 L0, 2 L1, 1 L4, 1 L6; stdout 245, stderr 244 aynı | h7b'ye göre önceden bilinen g06 stderr farkı. V6 ile 244 JSON aynı; q09'da yalnız rastgele rowid sayıları değişiyor. İkili yolu karşılaştırmadan çıkarıldı. |
| Çalışma alanı testleri | kapalı 1037 geçti / 0 hata; açık 1022 geçti / 15 bilinen hata; iki durumda 337 yok sayıldı | Beş yeni test geçiyor; başarısız test adları V6 ile aynı. Beklentiler değiştirilmedi. |
| Hız, Rust1.100 tabanı | toplam 298,51→298,31ms (**0,999x**); geomean 0,996x; 14 özet aynı | W06 tabana göre1,023x; önceki V6'ya karşı ayrıca15 denemede 1,020x. |

**Hız açığı:** W06 önceki V6 ikilisine göre son 15 tekrarda 10,87→11,08ms (**yaklaşık %2 gerileme**; `res/speedbench_v6_w06.txt`). Ara ölçümde yaklaşık %3,4 de görüldü. Kaynak nedeni henüz belirlenmedi; toplamın aynı kalması bu açığı kapatmaz. Hız kapısı ölçüldü, tüm iş yükleri için gerilemesiz sayılmadı. Hız/derleme ayarı uygulanmadı.

Anahtar açıkken kalan 15 testin adları `res/gate_comparison.json` içinde tek tek kaydedildi; önceki M1/M3 ve F4/F6–F9 sınıflarıyla aynı küme. Bunlar bu dilimin yeni hataları olarak sunulmuyor; tüm testler geçmiş gibi de sayılmıyor. Veri, ileti ve dosya baytı farkları, önceden bilinse de açık kusur olarak kalıyor.
