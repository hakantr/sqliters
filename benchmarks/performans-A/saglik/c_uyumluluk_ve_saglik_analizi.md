# Rust'ın yazdığı dosya ile C SQLite'ın yazdığı dosyanın karşılaştırması (bayt eşitliği ve fiziksel sağlık)

- **Ölçüm**: 2026-11-12, `Intel Xeon E5-2680 v4`, tmpfs. Rust = `performans-A@40e275d` (adım 4) CLI; C = SQLite `3.54.1` (`target/sqlite-ref/sqlite3_o3`).
- **Yöntem**: `gen.py` 13 belirlenimci SQL senaryosu üretir; `health.py` her senaryoyu iki motorla yazar ve dosyaları C'nin `dbstat` sanal tablosu, iki motorun `PRAGMA integrity_check`'i ve `VACUUM INTO` ile karşılaştırır. Ham sonuç: `sonuc_step4.json`.
- **Kapsam notu**: Buradaki Rust–C farklarının hiçbiri `performans-A` adımlarından gelmiyor; adım 1–4 dosyaları `performans@4ccdc64` tabanının yazdığıyla bayt bayt aynı (aşağıdaki rastgelelik durumu hariç).

## 1. Özet

| Soru | Sonuç |
| :--- | :--- |
| Rust dosyası C dosyasıyla bayt bayt aynı mı? | **Hayır**, 13 senaryonun hiçbirinde. |
| Mantıksal içerik aynı mı? | Evet, 13/13 (C her iki dosyadan aynı satırları okuyor). |
| C `integrity_check` Rust dosyasını kabul ediyor mu? | 13/13 `ok`. Ayrı 3 tekrar betiğinde **hayır** (bkz. §4). |
| Rust `integrity_check` C dosyasını kabul ediyor mu? | 13/13 `ok`. |
| Rust aynı betikle hep aynı dosyayı mı yazıyor? | 13/13 evet; aynı işlemde iki sanal tablo değişince **hayır** (bkz. §5). |

## 2. Senaryo tablosu (Rust / C)

| Senaryo | Sayfa | Doluluk % | Derinlik | Sıralılık % | Şişme (`/VACUUM`) |
| :--- | ---: | ---: | ---: | ---: | ---: |
| `s01` tek sayfa (60 satır) | 2 / 2 | 18,8 / 18,8 | 0 / 0 | 100 / 100 | 1,00 / 1,00 |
| `s02` sıralı ekleme (20 bin) | 233 / 233 | 98,5 / 98,5 | 1 / 1 | 100 / 100 | 1,00 / 1,00 |
| `s03` rastgele sırada ekleme | 259 / 260 | 88,9 / 88,5 | 1 / 1 | 7,0 / 5,1 | 1,112 / 1,116 |
| `s04` 3 indeksli ekleme | 488 / 488 | 92,6 / 92,6 | 2 / 2 | 8,6 / 8,6 | 1,066 / 1,066 |
| `s05` yüklemeden sonra `CREATE INDEX` | 490 / 458 | 92,1 / 98,5 | 2 / 1 | 100 / 100 | 1,070 / 1,000 |
| `s06` karışık ekle/sil/güncelle, 4096 | 296 / 249 | 62,4 / 74,1 | 2 / 2 | 39,7 / 27,9 | 1,542 / 1,297 |
| `s07` karışık, 512 bayt sayfa | 2456 / 2173 | 64,3 / 72,3 | 4 / 4 | 12,3 / 12,8 | 1,480 / 1,309 |
| `s08` karışık, 1024 + `auto_vacuum=FULL` | 1182 / 972 | 60,6 / 73,3 | 3 / 3 | 26,0 / 16,6 | 1,608 / 1,322 |
| `s09` karışık, WAL | 281 / 244 | 62,7 / 72,2 | 2 / 2 | 36,4 / 19,6 | 1,536 / 1,333 |
| `s10` `WITHOUT ROWID` | 318 / 318 | 75,0 / 75,0 | 2 / 2 | 4,2 / 4,2 | 1,298 / 1,298 |
| `s11` taşma sayfaları | 1601 / 1599 | 72,1 / 72,6 | 3 / 3 | 2,2 / 7,1 | 1,116 / 1,114 |
| `s12` R-Tree ekle/sil/güncelle | 65 / 73 | 86,5 / 79,2 | 1 / 1 | 100 / 58,2 | 1,000 / 1,090 |
| `s13` R-Tree yalnız ekleme | 79 / 81 | 88,1 / 86,0 | 1 / 1 | 100 / 61,3 | 1,000 / 1,025 |

- *Doluluk*: `1 − kullanılmayan bayt / (sayfa × sayfa boyutu)`. *Sıralılık*: anahtar sırasında ardışık yaprakların ardışık sayfa numarasında olma oranı. *Şişme*: sayfa sayısı / C `VACUUM INTO` sonrası sayfa sayısı.
- R-Tree düğüm sayısı: `s12` 147 / 152, `s13` 183 / 183; ağaç derinliği ikisinde de 2; `rtreecheck` ikisinde de `ok`.

## 3. Farkların niteliği

1. **Dosya değişim sayacı (başlık ofset 24–27 ve 92–95)**: Rust her zaman C'den tam 1 fazla. Boş veritabanındaki ilk ifade Rust'ta sayacı 2, C'de 1 artırıyor. En basit durumda (`s01`) tek fark bu 2 bayt.
2. **Sayfa içi boş alanın içeriği**: Rust boş bölgeyi sıfırlıyor, C eski baytları bırakıyor. `s02`de bunun dışındaki tüm canlı içerik (başlıklar, hücre işaretçileri, hücreler) aynı ofsetlerde ve aynı.
3. **Sayfa numaralandırması / bölünme kararları**: `s04`, `s10`da sayfa sayısı ve doluluk aynı ama sayfa numaraları ve yaprak içerikleri farklı (bölünmede hangi kardeşin hangi sayfayı aldığı ve hücre dağılımı farklı).
4. **Yapısal fark**: `s05`–`s09`, `s11`–`s13`te ağaçların kendisi farklı (sayfa sayısı, doluluk, derinlik).

## 4. Sağlık değerlendirmesi

**Rust'ın daha iyi olduğu noktalar**
- Boş alan sıfırlanıyor: silinmiş verinin artıkları dosyada kalmıyor (C'de `secure_delete` kapalıyken kalır).
- R-Tree: daha az sayfa (65'e 73), daha az düğüm (147'ye 152), gölge tablolar tamamen sıralı (100%'e 58%). Neden: düğümler bellekte tutulup commit'te düğüm numarası sırasıyla yazılıyor.
- Karışık yüklerde yaprak sıralılığı daha yüksek (`s06` 39,7'ye 27,9; `s08` 26,0'a 16,6; `s09` 36,4'e 19,6).
- Rastgele eklemede doluluk bir miktar daha iyi (88,9'a 88,5).

**Rust'ın daha kötü olduğu noktalar**
- **Silme/güncelleme sonrası tablo doluluğu**: `s06`da `t` tablosu 192 sayfa %48,9 dolu, C'de 147 sayfa %63,8. Dosya `s06`–`s09`da %13–22 daha büyük; şişme 1,48–1,61'e karşı C 1,30–1,33. İndeksler benzer (%88–90). Yani boşalan tablo yaprakları kardeşleriyle C kadar birleştirilmiyor.
- **Yüklemeden sonra `CREATE INDEX`**: indeks doluluğu %87–89, C'de %97,6–99,1; `ib` indeksi 142 sayfa/derinlik 2, C'de 126 sayfa/derinlik 1 (bir seviye fazla → arama başına bir sayfa okuması fazla).
- **Taşma sayfalı yükte sıralılık** 2,2'ye 7,1.
- Değişim sayacının 1 fazla olması zararsız, ama ilk ifadede fazladan bir commit yapıldığını gösteriyor.

**Hata (sağlıksız dosya)**
- `dengesiz_agac_tekrar_betikleri/` altındaki 3 betikte (512 bayt sayfa, kütüphane API'siyle çok sayıda R-Tree güncelleme/silme) Rust **yaprakları farklı derinlikte olan bir B-tree** yazıyor: C `PRAGMA integrity_check` → `Tree N page N cell K: Child page depth differs`. Örnek: `t_node` gölge tablosunun yaprakları derinlik 1 ve 2'de.
- Rust'ın kendi `integrity_check`'i aynı dosyaya `ok` diyor (bu kontrolü yapmıyor).
- `performans@4ccdc64` tabanıyla da aynı; `performans-A` adımlarından gelmiyor. C dosyayı okuyabiliyor, `rtreecheck` `ok`.
- Çalıştırma: `target/perf-work/step5/driver_base/target/release/driver_base <db> <betik>`.

## 5. Rastgele sayfa sırası (belirlenimsizlik)

- Aynı işlemde iki sanal tablo (ör. iki R-Tree) değiştiğinde aynı ikili aynı betikle iki farklı dosyadan birini yazıyor (`rtree_rastgele_sayfa_sirasi_tekrar.sql`; taban, adım 4 ve adım 5 adayı için 8'er koşuda aynı iki md5).
- İki varyant yalnızca gölge tabloların sayfa tahsis sırasında ayrışıyor: ağaç konumuna göre eşlenince 268 yaprağın hepsi bayt bayt aynı, 4 iç sayfa yalnızca çocuk sayfa numaralarında farklı.
- Tek R-Tree tablosu, yalnız otomatik commit ve yalnız ekleme varyantlarında dosya hep aynı.
- Olası neden (kod okunarak; adım 5 ajanı doğruluyor): `VTabRegistry.connected` rastgele tohumlu `HashMap`; işlemdeki sanal tabloların sync/commit sırası bu haritanın gezinme sırasına bağlı. C'de sıra, tabloların işleme katılma sırasıdır (`db->aVTrans`).
