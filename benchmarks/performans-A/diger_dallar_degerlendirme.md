# Diğer dalların değerlendirmesi (`performans-swar`, `-simd`, `-asm`, `-morsel`, `-io-mmap`, `-pgo`, `-hibrit`, `-1`)

- **Ölçüm**: 2026-11-13, `Intel Xeon E5-2680 v4`, tmpfs, tek çekirdeğe sabit, sıralı (interleaved) A/B, 7 deneme, medyan.
- **Karşılaştırma tabanı**: `performans-A@e4cdaff` (adım 5f). C referansı: SQLite `3.54.1 -O3`.
- **Derleme**: her dal `git archive` ile ayrı dizine çıkarılıp düz `cargo build --release` ile derlendi. **PGO uygulanmadı** (`llvm-profdata` bu makinede kurulu değil); `pgo` ve `hibrit` sütunları bu yüzden yayımlanmış sonuçlarından farklı bir derlemeyi ölçer (`*`).
- Tüm dallarda 14 çıktı özeti C ile eşit.

## 1. Bu makinede süreler (`ms`)

| İş yükü | C | A (5f) | swar | simd | asm | pgo* | io-mmap | morsel | hibrit* | performans-1 |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | 9,60 | 7,45 | 7,48 | 7,92 | 7,62 | 7,80 | 7,43 | 7,42 | 7,14 | 7,17 |
| `W02_create_indexes` | 8,76 | 7,61 | 8,23 | 9,35 | 8,44 | 9,48 | 9,10 | **3,42** | **3,38** | 3,54 |
| `W03_pk_point_select` | 23,30 | 27,74 | 36,81 | 36,47 | 36,76 | 37,35 | **24,87** | 37,22 | **24,53** | 24,97 |
| `W04_secondary_idx_range` | 7,46 | 7,79 | 14,41 | 14,36 | 14,26 | 14,65 | 12,15 | 10,72 | 7,73 | 6,78 |
| `W05_full_scan_filter_agg` | 16,57 | 4,39 | 18,01 | 17,26 | 17,01 | 17,47 | 18,04 | **2,28** | **2,09** | 1,22 |
| `W06_join_group_order` | 28,23 | 10,46 | 10,61 | 10,71 | 10,52 | 10,97 | 10,74 | 10,20 | 10,40 | 10,08 |
| `W07_window_functions` | 50,21 | 38,69 | 38,62 | 38,53 | 38,35 | 39,74 | 38,51 | 36,30 | 35,57 | 17,52 |
| `W08_indexed_update_delete` | 7,96 | **9,39** | 16,42 | 17,35 | 16,78 | 17,73 | 16,55 | 12,95 | 10,37 | 10,45 |
| `W09_like_text_scan` | 18,92 | 11,79 | 10,72 | 10,52 | **9,43** | 12,04 | 11,85 | 11,93 | 9,81 | 8,15 |
| `W10_jsonb_extract_agg` | 10,93 | **11,07** | 30,54 | 30,02 | 30,59 | 31,28 | 30,30 | 29,02 | 18,28 | 7,39 |
| `W11_fts5_index_and_bm25` | 57,02 | 45,85 | 46,86 | 47,24 | 47,21 | 50,64 | 47,90 | 46,41 | 47,09 | 44,55 |
| `W12_rtree_insert_and_bbox` | 24,63 | **23,82** | 40,07 | 37,79 | 37,14 | 40,81 | 41,07 | 26,73 | 24,57 | 25,11 |
| `W13_autocommit_wal_txns` | 4,12 | **4,83** | 6,49 | 6,54 | 6,59 | 7,37 | 5,59 | 7,00 | 5,00 | 4,82 |
| `W14_cli_sql_script` | 26,19 | 35,30 | 37,20 | 36,84 | 37,25 | 37,33 | 36,70 | 33,81 | 33,21 | 29,87 |
| **Toplam** | **293,9** | **246,2** | 322,5 | 320,9 | 317,9 | 334,7 | 310,8 | 275,4 | **239,2** | 201,6 |
| Saf Rust tabanına (`4ccdc64`, ~327 ms) oran | | 0,75x | 0,99x | 0,98x | 0,97x | 1,02x | 0,95x | 0,84x | 0,73x | 0,62x |

Kalın: `performans-A`ya göre o iş yükünde belirgin biçimde önde olan (Format-3 uyumlu) dal, ya da `performans-A`nın önde olduğu yer.

## 2. Bulgular

1. **`performans-A` (saf, güvenli Rust + doğruluk düzeltmeleri) bu makinede PGO'suz hibrit dalla aynı düzeyde**: 246,2 ms'ye 239,2 ms. Hibritin öne geçtiği yerler `W02`, `W05`, `W03`, `W09`, `W07`, `W14`; `performans-A`nın öne geçtiği yerler `W10`, `W08`, `W13`, `W12`.
2. **Yayımlanmış kazançların çoğu bu makinede yeniden üretilmedi.** Aynı tabana göre: `swar` 0,99x (yayımlanan 0,88x), `simd` 0,98x (0,83x), `asm` 0,97x (0,98x), `io-mmap` 0,95x (0,82x), `morsel` 0,84x (0,68x), `hibrit` 0,73x PGO'suz (0,525x PGO'lu). Olası nedenler: farklı işlemci (Broadwell'e karşı Zen 4), PGO'nun burada uygulanmaması ve yayımlanan ölçümlerin sıralı (interleaved) olmaması. Bir gözlem ikinci nedeni destekliyor: `swar` raporunda `W07` 22,5 → 16,6 ms görünüyor, oysa pencere fonksiyonları SWAR çekirdeklerinin dokunduğu bir yol değil ve burada `W07` değişmiyor (38,6'ya 38,7). Bu bir varsayım; Ryzen makinede sıralı ölçümle doğrulanmadı.
3. **SWAR / SIMD / ASM'nin bu makinede ölçülebilir etkisi dar**: `LIKE` taraması (`W09` 0,80–0,91x), WAL sağlama toplamı (`W13` taban 7,0 → 6,5) ve SIMD/ASM'de R-Tree (`W12` taban 40,3 → 37,1–37,8). `W12` ve `W13`te `performans-A` zaten daha hızlı (23,8 ve 4,8 ms).
4. **Dalların hiçbiri `performans-A`daki doğruluk düzeltmelerini içermiyor** (hepsi `4ccdc64`ten ayrılıyor). Hibrit ikilisinde doğrulandı: `PRAGMA secure_delete` yok sayılıyor; büyük indeks anahtarında `cells exceed usable B-tree page capacity`; C'nin yazdığı FTS5 dosyasında `MATCH 'alpha'` 643 satır (C: 710); `ALTER TABLE ADD COLUMN … DEFAULT x'00ff'` varsayılanı kayboluyor. Dengesiz ağaç ve WAL çökme açığı da tabandan geliyor.
5. **Hibrit ve morsel, dosyaları C'den uzaklaştırmıyor**: 203 senaryoda son dosya katmanı dağılımı tabanla aynı (bir senaryo daha iyi, hiçbiri daha kötü). `performans-A` 11 senaryoda daha yakın.

| Son dosya katmanı (203 senaryo) | L0 | L1 | L2 | L3 | L4 | L5 | L6 | çıktı = C |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| taban `4ccdc64` | 0 | 8 | 30 | 14 | 45 | 56 | 50 | 197 |
| `performans-hibrit` | 0 | 8 | 31 | 14 | 44 | 56 | 50 | 197 |
| `performans-morsel` | 0 | 8 | 31 | 14 | 44 | 56 | 50 | 197 |
| `performans-A` (5f) | 0 | 18 | 30 | 14 | 45 | 57 | 39 | 200 |

6. **Doğrudan birleştirme pratik değil**: hibrit ve `performans-A` aynı dosyaları ağır biçimde yeniden yazıyor (`btree/lib.rs`, `fast_read.rs`, `core/lib.rs`, `rtree.rs`; `fts5.rs` `performans-A`da baştan sona değişti). Yöntemler teker teker `performans-A` üzerine taşınmalı ve aynı kapılardan (C `integrity_check`, ayrışma katmanları, karar eşliği) geçirilmeli.
7. `unsafe` yalıtımı dallarda korunmuş: üst katman crate'lerinde `unsafe` eklenmemiş; SIMD ve assembly iki ayrı yaprak crate'te (`sqliters-simd` 97, `sqliters-asm` 113 `unsafe` satırı), `io-mmap` `vfs/sys` içinde 8 satır ekliyor. Hibrit iki entegrasyon testindeki bağımlılık kuralını bu iki yaprak crate'e izin verecek şekilde gevşetiyor.
8. `performans-1` Format-3 dışı kayıt düzeni kullandığı için C ile dosya uyumu hedefinin dışında.

## 3. İleriye götürülebilecek yöntemler ve dal planı

Tahmini kazançlar bu makinedeki `performans-A` süresine göre ve yalnızca yukarıdaki ölçümlerden türetilmiştir; taşındığında aynı kazancın elde edileceği ölçülmedi.

| Dal | Taşınacak yöntem | Hedef iş yükü (A → dalın değeri) | Saf Rust kısıtı |
| :--- | :--- | :--- | :--- |
| `performans-A-morsel` | arena + sıralama anahtarlı `CREATE INDEX`; yaprak içi üst sınır araması ve dilim toplama; yığınsız satır / kısa metin | `W02` 7,6 → 3,4; `W05` 4,4 → 2,1–2,3; `W07` 38,7 → 36,3; `W14` 35,3 → 33,8 | içinde |
| `performans-A-io` | değişmeyen WAL başlığında okuma kilidi kısa devresi; tek-TLS oturum kapısı | `W03` 27,7 → 24,5–24,9 | içinde (`mmap` hariç; `mmap` burada kazanç göstermedi) |
| `performans-A-pgo` | profil güdümlü derleme | ölçülmedi (araç eksik) | kod değişmiyor |
| `performans-A-simd` | `LIKE`/`GLOB` çekirdeği | `W09` 11,8 → 9,4–10,7 | dışında |
| `performans-A-hibrit` | yukarıdakilerden kanıtlananların birleşimi | — | karışık |

- `performans-A-io`, WAL okuma kilidi protokolüne dokunduğu için karar eşliği denetimi (C'nin `walTryBeginRead` kararlarıyla olay olay karşılaştırma) zorunlu.
- `performans-A-morsel`deki `CREATE INDEX` yolu, bedelli hizalamalardaki "indeks doluluğu" maddesiyle aynı kodu değiştiriyor; önce `performans-A-hizalama` sonuçlanmalı.
- `asm` için ayrı dal önerilmiyor (bu makinede `simd`den anlamlı farkı yok ve yayımlanan sonuçta da kazanç yok); `performans-1` uyum hedefinin dışında.
