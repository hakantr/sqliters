# V6b.34 — D142 ve sayfalayıcı eşleme yaşamı

B-tree imleçleri Unix VFS'nin xFetch yoluna bağlandı. Önceki altı D142 örneği artık ham karşılaştırmada eşit. Her edinim kendi kimliğini ve VFS lease'ini tutar. Bir imleci sıfırlamak veya kapatmak aynı sayfadaki başka bir imlecin referansını ya da PCache referansını bırakmaz.

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Ölçülen 17 C yordamı/blokta 150 gcov dalının 103'ü alındı. [Dal tablosu ve alınmayan 47 çıkış](karar_esligi_v6b_34_dallar.md).

## C kararları

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt sınırı |
|---|---|---|
| `pager.c:3587–3608`, açık ve iVersion≥3 dosya, `szMmap>0` | Getter seçimi istekle değişir. fd ayrı bir MMAP_SIZE hint'i alır. | `sqliters-pager/src/lib.rs:2755–2785` istek, getter ve kabul edilen fd sınırını ayrı tutar. Yerleşik Unix fd limitiyle aktif okuma ve hazırlama snapshot'u karşılaştırıldı. |
| `os_unix.c:4243–4250`, `nFetchOut>0` veya aynı/negatif sınır | fd güncelleme yapmaz. Mevcut lease bitince yeni bir SET isteği kabul edilebilir. | `sqliters-vfs/src/fetch.rs` lease sayacı ve `Pager::predict_mmap_size`. İki okuyucu, mapped+normal ref, hazırlama, shrink, reset ve finalize akışları. Red sonrası sıraya alınan otomatik tekrar yoktur. |
| `pager.c:5717–5729`, `pgno>1` ve `READER\|READONLY` | Page 1 PCache'de kalır. Sıfır sayfa CORRUPT döndürür. | `pin_btree_page`, `pin_readonly_page` ve `pin_mapped_page`. Page 1, okuyucu ve writer/READONLY alındı. C'de pgno=0 yönü bu profilde alınmadı. |
| `pager.c:5735–5744`, WAL aramasında frame ya da hata | Frame varsa normal getter WAL görüntüsünü görür. Hata xFetch'ten önce döner. | `pin_mapped_page`, ardından normal `pager_get`. WAL frame'li ve framesiz okumalar karşılaştırıldı. Bozuk WAL indeksi ve arama hata enjeksiyonu açık. |
| `pager.c:5744–5767`, xFetch sonucu ve TEMP/yazma durumu | xFetch önce gelir. TEMP veya yazma sırasında PCache hit'i varsa yeni lease bırakılır ve cache ref alınır. Disk READER'da PCache hit'i eşlemeyi gölgelemez. NULL xFetch normal getter'a düşer. | `pin_mapped_page` ve `page_lookup`. Dışarıda kalan son dosya sayfası ve 8192/8447/8448 bayt limitleri karşılaştırıldı. Mapped+PCache aynı numara ve cache sayaçları ayrı Rust testiyle doğrulandı. xFetch I/O hata enjeksiyonu açık. |
| `pager.c:4124–4177,5821–5869`, mapped PgHdr edinimi ve bırakma | Her edinim ayrı ref alır. Mapped ref PCache ref toplamı, hit/miss veya LRU'ya girmez. | `PageSource`, `Pager::pinned_page` ve `unpin_page`. İmleçler sayfa numarasına ek olarak kaynağı taşır. Rust'ın sahipli kopyası ile C'nin doğrudan işaretçisi farklı temsillerdir. C freelist tahsis izinin ve VFS byte kopyasının NOMEM eşliği açık. |
| `btree.c:2424–2446,4771–4783,5325–5348`, imleç bayrağı ve b-tree başlatma | Salt okunur imleç READONLY ister. Her yeni mapped PgHdr başlatılır. Overflow okuması yalnız doğrudan I/O olamadığında pager getter'ından ref alır. | `btcursor.rs` ve `pins.rs` kaynak kimliğini korur. Mapped b-tree sayfası `cell_size_check` varken her yeni edinimde kontrol edilir. Normal slice yardımcıları asıl imleçten bağımsız lease tutmaz. |
| `pager.c:5533–5538,5846–5869`, son page 1 ref ve read-lock kapanışı | Mapped ref'ler page 1 bırakılırken bitmiştir. | `set_state`, `finish_tx_to_read_or_open`, `unlock_db_file` ve `rollback_to_subsavepoint`. Cache reset ve geometri değişimi imleç epoch'unu yeniler. Reset/finalize, rollback ve savepoint akışları karşılaştırıldı. Genel pager ERROR durumu E28 kapsamındadır. |
| `pager.c:3310–3318,5478–5491`, WAL snapshot veya dosya sürümü değişimi | Cache temizlenir ve eski eşleme yenilenmek üzere unmap edilir. | `begin_read_under_shared_lock` ve `invalidate_mapping`. Rollback journal ve WAL API akışları ölçüldü. Dış süreçle truncate ve yeniden büyüme yarışı ayrıca hata enjeksiyonuna alınmadı. |
| `pager.c:3738–3753`, yeni TEMP fd | Fiziksel geçici dosya ilk yazmada açılır. Önceden saklanan mmap isteği burada fd'ye taşınmaz. | V6b.32 açılış davranışı korunur. `fix_mmap_limit` açılmış fd ve rollback geometri yenilemesinde çağrılır. 270 TEMP akışında aynı sonuç. |
| `os_unix.c:3999–4001,5718–5731`, truncate ve EOF256 | Fiziksel dosya kısalınca mevcut eşlemenin kullanılabilir boyutu kısalır. xFetch son 256 baytı aralık dışında tutar. | `fetch.rs` kullanılabilir ve munmap boyutlarını ayrı tutar. Aynı fixture'dan 13 adımlı shorten/regrow/unmap probu tam eşit. |
| `pager.c:4502–4511,6034–6037` ve `wal.c:2360–2379`, dosya büyümesi | Rollback yazımında `dbHintSize` ve dirty liste, WAL'da sync sonrası gerçek dosya boyutu SIZE_HINT'i belirler. | `Pager::pager_write_pagelist` ve `wal.rs:2539–2542,2661–2663` büyüme hint'ini C'nin ilgili yazımından önce verir. Ana dosya büyümesi ilk eşlemeden sonra 36 akışta karşılaştırıldı. |
| `os_unix.c:4094–4105,5674–5693`, Unix SIZE_HINT | Varsayılan chunk sıfırken fd uzar. Aktif xFetch varsa harita yerinde kalır, ref'ler kalktıktan sonraki yeni hint eşlemeyi büyütebilir. | `fetch.rs:109–125`. 17 doğrudan VFS işleminde dosya boyutu, fetch bytes ve sonuç aynı. Harita büyüklüğü eşleşir. Adres yeniden kullanımı, mremap syscall sırası ve mmap hata uyarısı açık. |

## Yürütme kapsamı

OFF modunda derleme anahtarını genel olarak açmadan, çalıştırma anında yalnız mmap etkin tek bir rowid tablosunun kontrol edilen C programı seçilebilir. `mapped_table_read_program` destekli opcode kümesini, tabloyu, sonucu ve imleç sayısını denetler. Kabul kümesinde salt projeksiyon, rowid aralığı, `COUNT(*)` ve BINARY sıralama var. UTF-16 kabulü bu yolun denetlenen BINARY sorter anahtarına uzanır. Önceki genel UTF-16 makine ve bağlı LIKE/GLOB hazırlama korumaları yerinde. Hazırlama yoklaması uygulama vtab callback'lerini çağırmaz. Diğer legacy planların mmap ömrü için genel eşlik sonucu çıkarılmadı.

## Doğrudan karşılaştırmalar

| Denetim | Sonuç |
|---|---|
| Önceki native mmap matrisi | 246/250. Altı D142 örneği eşit, dört D144 offset farkı ham çıktıda duruyor. |
| Genişletilmiş native mmap | 433/440. Yedi sapma V6b.33 ikilisiyle aynı ham çıktıya sahip. |
| Kalıcı dosyalar | Her iki native matriste bütün dosya SHA-256 değerleri eşit. |
| TEMP / tam program / elle CLI | 270/270, 144/144, 16/16. |
| Etkin okuyucu varken hazırlanan mmap snapshot | 12/12. Altı tam SET programı ve altı EXPLAIN listesi, API ve dosya hashleriyle birlikte eşit. Her akışta dondurulmuş GET programı da eşit. |
| Ek büyüme ve geometri matrisi | 52/52. Başlangıçta 36 büyüme akışı farklıydı. SIZE_HINT kararından sonra hepsi eşit. Bütün dosya hashleri eşit. |
| VFS | 10.051 önceki fetch/limit işlemi ve 13 truncate/remap ve 17 SIZE_HINT/remap işlemi eşit. |

## Dört kapı

| Kapı | Sonuç |
|---|---|
| Çalışma alanı OFF | 1135 geçti, 4 başarısız, 337 yok sayıldı. |
| Çalışma alanı ON | 1121 geçti, 18 başarısız, 337 yok sayıldı. |
| DML farksalı | OFF 378/400, ON 398/400. Bütün seed ve sınıf JSON'ları V6b.33 ile aynıdır. |
| Ayrışma bulucu | 245 senaryo. stdout 245, stderr 244 eşit. V6b.33 ile aynı ham özet. |
| Rust 1.100 hız tabanı | Yedi dönüşümlü ölçüm. Taban 299,67 ms, aday 367,01 ms. Aday/taban süre oranı 1,225. 14 digest eşit. |

Dört yeni test iki modda da geçti. Başarısız test adları V6b.33 ile aynı ve hiçbir eski beklenti değiştirilmedi.

İlk tam test denemesi kayıtlı imleçlerde kuşak numarasına göre yeni yığın biriktirdiği için durduruldu. Son kaynak canlı imleçleri yuva ile ayırır ve legacy yığını sonraki aynı yuva kullanımında yeniden kullanır. Dört kapı son kaynakla tekrar çalıştırıldı.

Ham kanıt ve araç kimlikleri yerel `/home/hakan/github/sqliters/target/perf-work/v6b34/` altında. `res/source_manifest.json`, `res/oracle_and_binaries.json`, `res/validation.txt` ve `res/verified_commit.txt` ölçümü kaynak ve Git kaydıyla bağlar. `mmap_known.py` eski dört sapmayı başlangıç kaydına karşı ham karşılaştırır. [Açık farklar](gorulen_farklar_v6b_34.md).
