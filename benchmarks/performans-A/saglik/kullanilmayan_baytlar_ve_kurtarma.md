# Kullanılmayan baytlar başka bir yerde kullanılıyor mu? (kurtarma, sağlama toplamları)

- **Ölçüm**: 2026-11-13, `Intel Xeon E5-2680 v4`, tmpfs. C = SQLite `3.54.1`; kurtarma için `-DSQLITE_ENABLE_DBPAGE_VTAB` ile derlenmiş ayrı bir C kabuğu (`.recover` komutu referans ikilide derli değil).
- **Soru**: Dosyada "kullanılmayan" sayılan baytlar (serbest bırakılmış sayfaların içeriği, sayfa içi serbest bloklar, hücre işaretçileri ile içerik arasındaki boşluk) C tarafında bir amaçla okunuyor mu?

## 1. Sonuç

| Bayt türü | C'de kim okuyor | Bu motorun durumu |
| :--- | :--- | :--- |
| **Serbest bırakılmış sayfaların içeriği** (serbest listedeki sayfalar) | **`.recover` / `sqlite3_recover`**: serbest listedeki sayfaları veri taşıyor olabilir diye tarar ve bulduğu satırları `lost_and_found` tablosuna yazar | Adım 5g'den beri C ile aynı (aşağıdaki deney). Öncesinde bu sayfalar sıfırlanıyordu ve **kurtarılabilir veri yok ediliyordu** |
| **Sayfa içi serbest bloklar** (canlı bir yapraktan silinen hücreler) | C'nin kendi kurtarması okumaz (`dbdata.c` yalnızca hücre işaretçi dizisini dolaşır). Üçüncü taraf adli bilişim araçları tarar | İçerik C'deki gibi yerinde kalıyor (`secure_delete` kapalıyken) |
| **Hücre işaretçileri ile içerik arasındaki boşluk** | Okuyan C kodu yok. C bu boşluğu `defragmentPage` içinde kendisi sıfırlar (`btree.c:1755`), diğer yeniden kurma yollarında eski baytları bırakır | Yeniden kurulan sayfalarda sıfır yazılıyor — kalan tek bayt farkı sınıfı (U1) |
| Tüm sayfa baytları (kullanılmayanlar dahil) | Sağlama toplamları: WAL çerçeve sağlama toplamı sayfanın tamamını kapsar (`wal.c:1018`); geri alma günlüğü sayfanın her 200. baytını örnekler (`pager.c:2289`) | Kullanılmayan baytlar farklıysa `-wal` / `-journal` içindeki sağlama toplamı değerleri de farklı olur; her dosya kendi içinde tutarlı, işlevsel etkisi yok |

## 2. Deney: `DROP TABLE` ve `DELETE` sonrası C `.recover`

Betik: 200 satırlık `keep`, 3.000 satırlık `gone`, 3.000 satırlık `part` tablosu; ardından `DROP TABLE gone`, `DELETE FROM part WHERE id % 3 = 0`, `DELETE FROM part WHERE id BETWEEN 1000 AND 1600`. Aynı betik üç motorla yazıldı; her dosya üzerinde C'nin `.recover --lost-and-found lf` komutu çalıştırıldı.

| Dosyayı yazan | Dosyada kalan `gone` satırı (ham bayt) | C `.recover`: `lost_and_found` toplam | silinen tablodan | silinen `part` satırlarından | canlı tablolar |
| :--- | ---: | ---: | ---: | ---: | :--- |
| C SQLite | 2.999 / 3.000 | **3.324** | 2.950 | 374 | `keep` 200, `part` 1.599 |
| `performans-A` adım 5h | 2.997 / 3.000 | **3.324** | 2.950 | 374 | `keep` 200, `part` 1.599 |
| `performans-A` adım 5f (hizalama öncesi) | 0 / 3.000 | **0** (`lost_and_found` oluşmuyor) | 0 | 0 | `keep` 200, `part` 1.599 |

- Adım 5g'deki "serbest bırakılan sayfalar yazılmaz" hizalaması, C'nin kurtarma davranışını geri getirdi: silinen tablonun 3.000 satırından 2.950'si kurtarılabiliyor ve sonuç C'nin yazdığı dosyayla aynı.
- `--ignore-freelist` verilirse C de bu satırları kurtarmaz (0 satır); yani bu veri gerçekten serbest liste sayfalarından geliyor.
- Canlı yapraklardan silinen satırlar (`id % 3 = 0`) `.recover` tarafından hiçbir dosyada kurtarılmıyor; yalnızca tamamen boşalıp serbest listeye giden yapraklardaki 374 satır geliyor.

## 3. Değerlendirme

- "Kullanılmayan" baytların kurtarma için anlamlı olan kısmı serbest bırakılmış **sayfalardır** ve bu artık C ile aynıdır.
- Kalan fark sınıfı (yeniden kurulan sayfalardaki boşluk baytları) C'nin kurtarması, bütünlük kontrolü ya da herhangi bir SQL çıktısı tarafından okunmuyor; yalnızca ham dosya baytlarında, `sqlite_dbpage` çıktısında ve sağlama toplamı değerlerinde görünür.
- Hizalama dalı için ek kapı: ağaç şekli değişiklikleri hangi sayfaların serbest kaldığını değiştirdiğinden, **C `.recover` çıktısı Rust'ın ve C'nin yazdığı dosyada aynı olmalı** koşulu doğrulama listesine eklenmiştir.
