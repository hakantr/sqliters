# `performans-A-hizalama` — C'ye Hizalama Adımları (Bedelli) Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-14T12:00:25+03:00`
- **Yöntem**: `speedbench` `W01`–`W14`; 1 ısınma + 9 sıralı (interleaved) deneme, medyan; süreçler `taskset -c 20` ile tek çekirdeğe sabit; veritabanı dosyaları `tmpfs (/run/user/1000/sqliters-perf)` üzerinde (fsync gürültüsü yok). Tüm aşamaların saklanmış ikilileri ve C referansı **aynı oturumda** ölçüldü.
- **Kısıtlar (tüm aşamalar)**: `#![forbid(unsafe_code)]`, sıfır dış bağımlılık, SWAR / SIMD / `asm!` yok, SQLite Format-3 ve WAL protokolü uyumu, C ile birebir `FNV-1a` çıktı özeti.

> **Bu dal ne için.** `performans-A` üzerinde C SQLite 3.54.1'e hizalanması hız bedeli olan sınıflar burada, her biri ayrı commit ve ayrı ölçüm aşaması olarak yapılır. Proje kuralı: aynı girdide çıktı C ile aynı olmalıdır; bu hizalamalar zorunludur, bedelleri sonradan optimizasyonla geri kazanılır.
>
> **Okuma.** `00_performans_A_ucu` satırı dalın ayrıldığı `performans-A` ucudur; `Geomean (vs taban)` ve `Bu / Taban` sütunları ona göredir. Her aşamanın bedeli `Bu / Önceki` sütununda görülür. Donanım: bu makine (Xeon E5-2680 v4); üst dizindeki `benchmark_report.md` ile karşılaştırılamaz.
>
> **Her aşamanın kapıları.** C `integrity_check`; ayrışma bulucu katmanları (kötüleşme yok); karar eşliği (izli C derlemesi ile olay olay karşılaştırma, tetiklenmeyen kararlar ayrıca listelenir); C `.recover` çıktısının Rust'ın ve C'nin yazdığı dosyada aynı olması; tam test paketi.

**Dosyalar**: her aşama için `NN_<aşama>.json` (deneme bazında süreler, RSS, özetler); `asamalar.json` (aşama tanımları, commit'ler, test sonuçları); `olc.py` (bu raporu üreten betik); `ek_resmi_run_speedbench_taban.json` (bazın `tools/run_speedbench.py` ile sabitlenmemiş, btrfs üzerindeki ilk resmi koşusu: Rust `723,80 ms` / C `647,26 ms`, geomean `1,12x` — fsync ve zamanlama gürültüsü içerir).

## 1. Aşama Özeti

| Aşama | Git (`dal@commit`) | İkili (`KiB`) | `.text` (`KiB`) | Toplam (`ms`) | Geomean (`vs taban`) | Geomean (`vs C`) | Toplam (`vs C`) | Tepe RSS (`KiB`) | Özetler |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`294.10`** | `—` | **`1.000x`** | **`1.000x`** | `6,364` | `—` |
| **00_performans_A_ucu** | `performans-A@225f5c2` | `7,299.0` | `6,157.8` | **`248.08`** | `1.000x` | **`0.823x`** | `0.844x` | `18,304` | 14/14 ✓ |
| **01_h1_silmede_dengeleme** | `performans-A-hizalama@034af4f` | `7,346.9` | `6,193.5` | **`247.88`** | `1.000x` | **`0.824x`** | `0.843x` | `18,388` | 14/14 ✓ |
| **02_h1b_dml_stratejisi** | `performans-A-hizalama@ebc3fde` | `7,461.8` | `6,294.8` | **`248.72`** | `1.003x` | **`0.826x`** | `0.846x` | `18,364` | 14/14 ✓ |
| **03_h2_eklemede_dengeleme** | `performans-A-hizalama@0c65dbe` | `7,376.3` | `6,223.2` | **`247.98`** | `0.996x` | **`0.820x`** | `0.843x` | `18,320` | 14/14 ✓ |
| **04_h3_toplu_kurma** | `performans-A-hizalama@fd7364d` | `7,363.6` | `6,217.5` | **`247.08`** | `0.982x` | **`0.809x`** | `0.840x` | `18,700` | 14/14 ✓ |
| **05_h4_rtree** | `performans-A-hizalama@b734244` | `7,420.3` | `6,267.2` | **`252.59`** | `0.999x` | **`0.822x`** | `0.859x` | `18,652` | 14/14 ✓ |
| **06_h5_fts5** | `performans-A-hizalama@ba3b169` | `7,516.1` | `6,347.9` | **`249.29`** | `0.986x` | **`0.812x`** | `0.848x` | `17,892` | 14/14 ✓ |
| **07_h6_onbellek_tasmasi** | `performans-A-hizalama@27eacad` | `7,588.6` | `6,409.6` | **`255.00`** | `1.009x` | **`0.831x`** | `0.867x` | `17,680` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_A_ucu | 01_h1_silmede_dengeleme | 02_h1b_dml_stratejisi | 03_h2_eklemede_dengeleme | 04_h3_toplu_kurma | 05_h4_rtree | 06_h5_fts5 | 07_h6_onbellek_tasmasi |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.60` | `7.57` (**0.79x**) | `7.53` (**0.79x**) | `7.40` (**0.77x**) | `7.58` (**0.79x**) | `7.87` (**0.82x**) | `7.68` (**0.80x**) | `7.63` (**0.79x**) | `8.42` (**0.88x**) |
| `W02_create_indexes` | `8.65` | `7.46` (**0.86x**) | `7.46` (**0.86x**) | `7.55` (**0.87x**) | `7.51` (**0.87x**) | `5.65` (**0.65x**) | `5.64` (**0.65x**) | `5.63` (**0.65x**) | `5.64` (**0.65x**) |
| `W03_pk_point_select` | `23.41` | `27.65` (**1.18x**) | `28.08` (**1.20x**) | `28.12` (**1.20x**) | `27.96` (**1.20x**) | `28.31` (**1.21x**) | `28.17` (**1.20x**) | `27.75` (**1.19x**) | `28.06` (**1.20x**) |
| `W04_secondary_idx_range` | `7.43` | `7.82` (**1.05x**) | `7.88` (**1.06x**) | `7.94` (**1.07x**) | `7.80` (**1.05x**) | `7.80` (**1.05x**) | `7.82` (**1.05x**) | `7.72` (**1.04x**) | `8.06` (**1.09x**) |
| `W05_full_scan_filter_agg` | `16.52` | `4.28` (**0.26x**) | `4.36` (**0.26x**) | `4.28` (**0.26x**) | `4.28` (**0.26x**) | `4.06` (**0.25x**) | `4.12` (**0.25x**) | `4.09` (**0.25x**) | `4.28` (**0.26x**) |
| `W06_join_group_order` | `28.26` | `10.78` (**0.38x**) | `10.68` (**0.38x**) | `10.96` (**0.39x**) | `10.65` (**0.38x**) | `10.82` (**0.38x**) | `10.81` (**0.38x**) | `10.63` (**0.38x**) | `10.53` (**0.37x**) |
| `W07_window_functions` | `50.12` | `38.69` (**0.77x**) | `38.35` (**0.77x**) | `38.51` (**0.77x**) | `38.77` (**0.77x**) | `37.96` (**0.76x**) | `37.59` (**0.75x**) | `37.78` (**0.75x**) | `38.28` (**0.76x**) |
| `W08_indexed_update_delete` | `8.00` | `9.55` (**1.19x**) | `9.81` (**1.23x**) | `9.78` (**1.22x**) | `9.38` (**1.17x**) | `9.74` (**1.22x**) | `9.71` (**1.21x**) | `9.65` (**1.21x**) | `9.91` (**1.24x**) |
| `W09_like_text_scan` | `19.01` | `11.88` (**0.62x**) | `12.09` (**0.64x**) | `11.96` (**0.63x**) | `11.86` (**0.62x**) | `12.09` (**0.64x**) | `12.14` (**0.64x**) | `12.11` (**0.64x**) | `11.81` (**0.62x**) |
| `W10_jsonb_extract_agg` | `10.94` | `11.27` (**1.03x**) | `11.12` (**1.02x**) | `11.22` (**1.03x**) | `11.15` (**1.02x**) | `10.99` (**1.00x**) | `11.22` (**1.03x**) | `10.98` (**1.00x**) | `11.62` (**1.06x**) |
| `W11_fts5_index_and_bm25` | `57.43` | `45.93` (**0.80x**) | `45.24` (**0.79x**) | `45.45` (**0.79x**) | `46.08` (**0.80x**) | `46.20` (**0.80x**) | `42.39` (**0.74x**) | `40.40` (**0.70x**) | `41.96` (**0.73x**) |
| `W12_rtree_insert_and_bbox` | `24.34` | `23.72` (**0.97x**) | `23.94` (**0.98x**) | `24.10` (**0.99x**) | `23.89` (**0.98x**) | `23.86` (**0.98x**) | `33.46` (**1.38x**) | `33.57` (**1.38x**) | `34.89` (**1.43x**) |
| `W13_autocommit_wal_txns` | `4.01` | `4.92` (**1.23x**) | `4.73` (**1.18x**) | `4.81` (**1.20x**) | `4.78` (**1.19x**) | `4.94` (**1.23x**) | `4.85` (**1.21x**) | `4.62` (**1.15x**) | `4.52` (**1.13x**) |
| `W14_cli_sql_script` | `26.39` | `36.55` (**1.39x**) | `36.60` (**1.39x**) | `36.64` (**1.39x**) | `36.30` (**1.38x**) | `36.80` (**1.39x**) | `36.98` (**1.40x**) | `36.72` (**1.39x**) | `37.01` (**1.40x**) |
| **Toplam** | **`294.10`** | **`248.08`** (**0.84x**) | **`247.88`** (**0.84x**) | **`248.72`** (**0.85x**) | **`247.98`** (**0.84x**) | **`247.08`** (**0.84x**) | **`252.59`** (**0.86x**) | **`249.29`** (**0.85x**) | **`255.00`** (**0.87x**) |

---

## 00_performans_A_ucu — `performans-A@225f5c2`

**Baz.** `performans-A` dalının bu dal ayrılırken ucu (adım 5j). Ayrışma bulucu, 234 senaryo, son dosya katmanı: L0 56, L2 43, L3 3, L4 58, L5 59, L6 15.

- **Testler**: `cargo test --workspace`: 757 geçti, bilinen 4 başarısız, 337 yok sayıldı.
- **Toplam**: `248.08 ms` (taban `248.08 ms`, C `294.10 ms`) — geomean `Rust/C` **`0.823x`** (disk WAL `0.796x`, bellek-içi `0.930x`)
- **Tepe RSS**: `18,304 KiB` (C: `6,364 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.57`** | `7.54` | `1.000x` | `9.60` | **`0.79x`** | `9,052` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.46`** | `7.43` | `1.000x` | `8.65` | **`0.86x`** | `10,020` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.65`** | `27.50` | `1.000x` | `23.41` | **`1.18x`** | `10,020` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.82`** | `7.69` | `1.000x` | `7.43` | **`1.05x`** | `10,084` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.27` | `1.000x` | `16.52` | **`0.26x`** | `10,084` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.78`** | `10.75` | `1.000x` | `28.26` | **`0.38x`** | `10,276` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.69`** | `38.44` | `1.000x` | `50.12` | **`0.77x`** | `12,700` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.55`** | `9.51` | `1.000x` | `8.00` | **`1.19x`** | `12,700` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.88`** | `11.80` | `1.000x` | `19.01` | **`0.62x`** | `12,700` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.27`** | `11.18` | `1.000x` | `10.94` | **`1.03x`** | `12,884` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.93`** | `45.18` | `1.000x` | `57.43` | **`0.80x`** | `18,172` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.72`** | `23.09` | `1.000x` | `24.34` | **`0.97x`** | `18,220` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.92`** | `4.88` | `1.000x` | `4.01` | **`1.23x`** | `18,252` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.55`** | `36.09` | `1.000x` | `26.39` | **`1.39x`** | `18,220` | `fc7490e2b80c68e2` ✓ |

---

## 01_h1_silmede_dengeleme — `performans-A-hizalama@034af4f`

**H1 — silmede C gibi yeniden dengeleme.** C, bir silmeden sonra sayfanın boş alanı kullanılabilir alanın üçte ikisini aşarsa `balance()` çağırır ve `balance_nonroot()` hücreleri en çok üç kardeş arasında yeniden dağıtır (gerekirse bir sayfayı serbest bırakır, kökte bir seviye düşürür); iç sayfadaki bir hücre silinince yerine öncül yaprak hücresi konur. Motor yalnızca yaprak tamamen boşaldığında davranıyordu; iç indeks hücresi silmede bazı durumlarda tüm indeksi yeniden kuruyordu. Yeni `sqliters-btree/src/balance.rs`, C'nin `balance` / `balance_nonroot` / `balance_deeper` / `balance_quick`, `insertCell` / `dropCell` / `allocateSpace` / `defragmentPage`, `editPage` / `rebuildPage` işlevlerinin portudur; tüm silme yolları (genel, konumlu, ham hızlı yollar, indeks) buna bağlandı. Hızlı yollar ham kaldı: üçte iki testi O(1) bir sınırla başlıyor. Ekleme tarafı bu adımda değişmedi (H2 aynı kodu etkinleştirecek). Motor düzeyinde bir düzeltme de gerekti: satır kimliği çakışmasında `REPLACE` artık C gibi yalnızca indeks girdilerini silip satırın üzerine yazıyor.

- **Testler**: `cargo test --workspace`: 764 geçti (7 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Karar eşliği** (iki motor aynı başlangıç dosyasından; 95 senaryo, 762.748 C olayı): 93 senaryoda her karar, SQL çıktısı, bütünlük ve sayfa sayfa şekil C ile aynı; kalan ikisi bu adımdan önce de var olan motor düzeyi farklar (indeks taramalı silmede ağaçlar arası yazma sırası; tetikleyici içi `DELETE`in satır ziyaret sırası). Hiç tetiklenmeyen kararlar: `balance_quick` ve `bBulk` dalları (yalnızca eklemede), bozuk girdi dalları. Ayrıntı: `karar_esligi_h1.md`. **Ayrışma bulucu** (245 senaryo, son derleme): kötüleşen yok, 24 iyileşti (L5 59 → 39, L2 44 → 55). **Sayfa sayıları (motor/C):** karışık 4096: 296 → 251 / 249; 512: 2456 → 2214 / 2173; 1024 + auto_vacuum: 1182 → 986 / 972; WAL: 281 → 244 / 244. Kalan fark ekleme tarafı bölünmelerden (H2). **Farksal koşu:** 4.800 tohum, çıktı C ile ve dal noktasıyla aynı, her dosya C `integrity_check` geçiyor. **Çökme/geri alma:** 288 betik + commit sırasında öldürülen 2.660 kurtarma, 0 hata. **Kurtarma eşliği (kapı tam karşılanmadı):** otomatik commit'te aynı şekilli 93 dosyanın 93'ünde C `.recover` çıktısı aynı; açık işlem içinde 99'un 84'ünde. Neden bu adımdan önce de var: motor açık işlemdeki her genel-yol ifadesi için ifade kayıt noktası açıyor (C yalnızca çok satırlı ve iptal olabilecek ifadelerde), bu yüzden serbest kalan sayfanın yazımı düşürülemiyor. **Bedel:** `W08` süre +%5,0 (ajanın koşusunda +%3,3), komut +%0,75; diğer iş yüklerinde komut sayısı aynı. Kalan maliyet: C sayfa başına boş alanı önbellekte tutuyor (`nFree`), port gerektiğinde yeniden hesaplıyor.
- **Toplam**: `247.88 ms` (taban `248.08 ms`, C `294.10 ms`) — geomean `Rust/C` **`0.824x`** (disk WAL `0.798x`, bellek-içi `0.924x`)
- **Tepe RSS**: `18,388 KiB` (C: `6,364 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.53`** | `7.48` | `7.57` | **`0.996x`** | `0.996x` | `9.60` | **`0.79x`** | `9,136` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.46`** | `7.42` | `7.46` | **`0.999x`** | `0.999x` | `8.65` | **`0.86x`** | `10,100` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.08`** | `27.39` | `27.65` | **`1.015x`** | `1.015x` | `23.41` | **`1.20x`** | `10,100` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.88`** | `7.67` | `7.82` | **`1.008x`** | `1.008x` | `7.43` | **`1.06x`** | `10,164` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.36`** | `4.35` | `4.28` | **`1.019x`** | `1.019x` | `16.52` | **`0.26x`** | `10,164` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.68`** | `10.65` | `10.78` | **`0.991x`** | `0.991x` | `28.26` | **`0.38x`** | `10,356` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.35`** | `38.12` | `38.69` | **`0.991x`** | `0.991x` | `50.12` | **`0.77x`** | `12,784` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.81`** | `9.75` | `9.55` | **`1.027x`** | `1.027x` | `8.00` | **`1.23x`** | `12,784` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.09`** | `11.99` | `11.88` | **`1.018x`** | `1.018x` | `19.01` | **`0.64x`** | `12,784` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.12`** | `11.08` | `11.27` | **`0.987x`** | `0.987x` | `10.94` | **`1.02x`** | `12,964` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.24`** | `44.57` | `45.93` | **`0.985x`** | `0.985x` | `57.43` | **`0.79x`** | `18,184` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.94`** | `23.41` | `23.72` | **`1.009x`** | `1.009x` | `24.34` | **`0.98x`** | `18,232` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.73`** | `4.48` | `4.92` | **`0.960x`** | `0.960x` | `4.01` | **`1.18x`** | `18,264` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.60`** | `36.09` | `36.55` | **`1.002x`** | `1.002x` | `26.39` | **`1.39x`** | `18,232` | `fc7490e2b80c68e2` ✓ |

---

## 02_h1b_dml_stratejisi — `performans-A-hizalama@ebc3fde`

**H1b — DML ifadelerinin C'nin kod üreticisi gibi yürütülmesi.** (A) **Geçiş stratejisi ve satır ziyaret sırası**: `DELETE` ve `UPDATE` için C'nin kararları port edildi (tek geçiş / satır kümesi / geçici indeks; `bComplex`, `chngKey`, `hasFK`, kısaltma optimizasyonu koşulları); iki geçişte satırlar C gibi satır kimliği/birincil anahtar sırasıyla işleniyor. Tetikleyicili tabloda indeks üzerinden `DELETE` artık C ile aynı sonucu veriyor (önce sonuç farklıydı). `AUTOINCREMENT` ifade başına sayaç kullanıyor; `last_insert_rowid` ve `total_changes` başarısız ifadede C gibi geri alınmıyor; tek satırlık `INSERT`te eksik üst satır hemen hata veriyor. (B) **Kısıt kontrol sırası ve yazma sırası**: NOT NULL → CHECK → satır kimliği → UNIQUE indeksler (C'nin indeks sırasıyla; önce ters sıradaydı); `STRICT` tür kontrolü C'nin konumunda; `REPLACE` çakışan indeks girdisini en son siliyor; üretilmiş sütunlar bağımlılık sırasıyla hesaplanıyor (ileri başvuruda yanlış indeks girdisi yazılıyor ve C `integrity_check` düşüyordu). (C) **İfade günlüğü** yalnızca C'nin açtığı durumda açılıyor (`isMultiWrite && mayAbort`); başarısız ifadenin neyi geri aldığı `sqlite3VdbeHalt` kurallarına bağlandı. Bir entegrasyon testi beklentisi C'ye göre değişti (başarısız commit'ten sonra `last_insert_rowid`).

- **Testler**: `cargo test --workspace`: 781 geçti (17 yeni), bilinen 4 başarısız, 337 yok sayıldı. **DML farksalı** (rastgele şema, tetikleyici, yabancı anahtar, çakışma yan tümceleri; CLI + API; her ifadeden sonra `changes`/`total_changes`/`last_insert_rowid`): ajanın 3.000 tohumunda C ile birebir aynı 2.839; bağımsız 400 yeni tohumda (yalnız CLI) **377** — H1 ve dal noktası 134. Dal noktasında görülen 2 C `integrity_check` hatası kalmadı. Kalan farklar (örnekle sınıflandı): SELECT planlayıcısından gelen satır sırası (görünümler, WITHOUT ROWID indeks seçimi), DDL hata metinleri, iki yabancı anahtar ayrıntısı, `INSERT…SELECT`in eş-yordam durumu. **Karar izi** (600 tohum, 6.487 ifade): günlük açma 0 uyuşmazlık, durdurma sonucu 0, strateji 28, günlük bayrakları 9; B-tree yazma dizisi 6.184 eşit / 303 farklı. **Kurtarma eşliği:** açık işlem içinde 99/100 aynı (H1: 84/99); kalan durum serbest sayfa yolu farkı. **H1 karar izi:** 95/95 tam eşit (H1: 93). **Ayrışma bulucu:** dağılım aynı, kötüleşen yok. **Çökme/geri alma:** yeni ifade-iptali takımı 84 betik + 432 kurtarma, diğer iki takım hızlı kipte, 0 hata. **Bedel:** toplam 1,007x; komut sayıları 0,999–1,001, yalnız `W12` +%0,9 ve `W11` +%0,3 (ifade başına günlük politikası çağrısı). `W06` +%3,8 süre aynı komut sayısıyla (kod yerleşimi). **Hizalanmayan:** WITHOUT ROWID `UPDATE` tüm indeks girdilerini siliyor/yeniden ekliyor (C yalnız değişenleri); UPSERT'te silme sırası; C'nin eş-yordamlı `INSERT…SELECT`i; DDL/ANALYZE/PRAGMA ifade günlüğü bayrakları; 9 bayrak uyuşmazlığının ~5'inde C'nin günlük açtığı yerde motor açmıyor (farksalda gözlenmedi, dışlanmadı).
- **Toplam**: `248.72 ms` (taban `248.08 ms`, C `294.10 ms`) — geomean `Rust/C` **`0.826x`** (disk WAL `0.800x`, bellek-içi `0.930x`)
- **Tepe RSS**: `18,364 KiB` (C: `6,364 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.40`** | `7.34` | `7.53` | **`0.983x`** | `0.978x` | `9.60` | **`0.77x`** | `9,272` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.55`** | `7.45` | `7.46` | **`1.013x`** | `1.012x` | `8.65` | **`0.87x`** | `10,108` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.12`** | `27.48` | `28.08` | **`1.001x`** | `1.017x` | `23.41` | **`1.20x`** | `10,108` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.94`** | `7.68` | `7.88` | **`1.007x`** | `1.015x` | `7.43` | **`1.07x`** | `10,172` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `4.36` | **`0.980x`** | `0.999x` | `16.52` | **`0.26x`** | `10,172` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.96`** | `10.93` | `10.68` | **`1.026x`** | `1.017x` | `28.26` | **`0.39x`** | `10,236` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.51`** | `38.38` | `38.35` | **`1.004x`** | `0.995x` | `50.12` | **`0.77x`** | `12,728` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.78`** | `9.74` | `9.81` | **`0.997x`** | `1.024x` | `8.00` | **`1.22x`** | `12,728` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.96`** | `11.81` | `12.09` | **`0.989x`** | `1.006x` | `19.01` | **`0.63x`** | `12,728` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.22`** | `11.17` | `11.12` | **`1.009x`** | `0.996x` | `10.94` | **`1.03x`** | `12,912` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.45`** | `45.13` | `45.24` | **`1.005x`** | `0.990x` | `57.43` | **`0.79x`** | `18,136` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.10`** | `23.52` | `23.94` | **`1.007x`** | `1.016x` | `24.34` | **`0.99x`** | `18,188` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.81`** | `4.60` | `4.73` | **`1.017x`** | `0.977x` | `4.01` | **`1.20x`** | `18,220` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.64`** | `36.02` | `36.60` | **`1.001x`** | `1.003x` | `26.39` | **`1.39x`** | `18,188` | `fc7490e2b80c68e2` ✓ |

---

## 03_h2_eklemede_dengeleme — `performans-A-hizalama@0c65dbe`

**H2 — eklemede sayfa bölünmesi, hücre yerleşimi ve sayfa tahsisi C gibi.** Her B-tree eklemesi ve yerinde satır değiştirme artık C'nin `sqlite3BtreeInsert` yolunu izliyor (yeni `sqliters-btree/src/insert.rs`): aynı boyutta üzerine yazma (`btreeOverwriteCell`), `fillInCell` (yerel/taşma bölümü, taşma zincirinin tahsis sırası), var olan hücrenin değiştirilmesi, `insertCell` → `allocateSpace` (serbest bloktan ilk uyan, boşluk ya da birleştirme) ve sayfa taşınca H1'de port edilen `balance()` (`balance_quick`, üç kardeş arası dağıtım, `balance_deeper`). Hızlı yollar ham kaldı: boşluğa sığan ekleme C'de de yalnızca `insertCell`dir. Motorun kendi bölünme ve yeniden paketleme kodu (yaklaşık 3.500 satır) kaldırıldı. C'nin bıraktığı baytlar artık temizlenmiyor (yeni kök yalnız başlık; serbest listeden alınan sayfa yalnız C'nin sıfırladığı durumda sıfırlanıyor). Motor düzeyinde üç kural C'ye getirildi: WITHOUT ROWID `UPDATE` yalnız değişen ikincil indeksleri yazıyor; indekssiz tabloda `REPLACE` ve eski satırı okumayan iki geçişli `UPDATE` C'nin hücre değiştirme yolunu kullanıyor; ikincil indeksi/tetikleyicisi/yabancı anahtarı olmayan WITHOUT ROWID tabloda `REPLACE` çakışan satırı silmiyor (bir entegrasyon testi beklentisi buna göre değişti).

- **Testler**: `cargo test --workspace`: 792 geçti (11 yeni; 10'u C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile **bayt bayt aynı 58 → 195**; kötüleşen yok, 139 iyileşti. Kalan 50'yi engelleyenler: indeks toplu kurma/`VACUUM` (17, H3), `ALTER TABLE` şema satırı dizisi (4), R-Tree (18), FTS5 (7), dört tekil. **Karar eşliği** (iki motor boş dosyadan; 180 betik, 3.092.603 C olayı): 176 betikte olay dizisi, SQL çıktısı, bütünlük ve şekil aynı, 175'i bayt bayt aynı; uyuşmayan 4 betik H3'ün konusu. `balance_quick` 14.520 kez tetiklendi (H1'de 0). Tetiklenmeyen: `bBulk=1` dalları SQL üzerinden (bir alt düzeyde C'nin `CREATE INDEX` şekliyle doğrulandı), `BTREE_PREFORMAT`. Ayrıntı: `karar_esligi_h2.md`. **Sağlık senaryoları:** s01–s04 ve s06–s09 C ile bayt bayt aynı; sayfa sayıları s03, s04, s06–s11'de C ile eşit. **`secure_delete`:** `ON` iken dosyada kalan artık sayısı C ile aynı (1.955; 1.932 canlı) — C bölünmeden sonra sayfa boşluğunda eski bir kopya bırakıyor, motor artık aynısını yapıyor. **Farksal koşular:** 4.000 tohum, çıktı C ile aynı, her dosya C `integrity_check` geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** üç kipte 99/100, 94/94, 99/100 ("her motor kendi dosyasını kurar" kipi önce 49/100 idi). **Çökme/geri alma:** 0 hata. **Bedel:** toplam 1,006x; komut sayıları `W08` 0,980, `W10` 0,946, `W13` 0,986, `W14` 0,992 (azaldı), `W01` 1,001, `W06` 1,006.
- **Toplam**: `247.98 ms` (taban `248.08 ms`, C `294.10 ms`) — geomean `Rust/C` **`0.820x`** (disk WAL `0.793x`, bellek-içi `0.930x`)
- **Tepe RSS**: `18,320 KiB` (C: `6,364 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.58`** | `7.55` | `7.40` | **`1.024x`** | `1.002x` | `9.60` | **`0.79x`** | `9,284` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.45` | `7.55` | **`0.994x`** | `1.006x` | `8.65` | **`0.87x`** | `10,184` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.96`** | `27.49` | `28.12` | **`0.994x`** | `1.011x` | `23.41` | **`1.20x`** | `10,184` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.80`** | `7.68` | `7.94` | **`0.982x`** | `0.997x` | `7.43` | **`1.05x`** | `10,248` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.27` | `4.28` | **`1.000x`** | `0.999x` | `16.52` | **`0.26x`** | `10,248` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.65`** | `10.59` | `10.96` | **`0.972x`** | `0.988x` | `28.26` | **`0.38x`** | `10,376` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.77`** | `38.47` | `38.51` | **`1.007x`** | `1.002x` | `50.12` | **`0.77x`** | `12,748` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.38`** | `9.35` | `9.78` | **`0.959x`** | `0.982x` | `8.00` | **`1.17x`** | `12,748` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.86`** | `11.84` | `11.96` | **`0.992x`** | `0.998x` | `19.01` | **`0.62x`** | `12,748` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.15`** | `11.10` | `11.22` | **`0.994x`** | `0.990x` | `10.94` | **`1.02x`** | `12,932` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.08`** | `45.43` | `45.45` | **`1.014x`** | `1.003x` | `57.43` | **`0.80x`** | `18,168` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.89`** | `23.42` | `24.10` | **`0.991x`** | `1.007x` | `24.34` | **`0.98x`** | `18,228` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.78`** | `4.65` | `4.81` | **`0.993x`** | `0.970x` | `4.01` | **`1.19x`** | `18,260` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.30`** | `36.04` | `36.64` | **`0.991x`** | `0.993x` | `26.39` | **`1.38x`** | `18,228` | `fc7490e2b80c68e2` ✓ |

---

## 04_h3_toplu_kurma — `performans-A-hizalama@fd7364d`

**H3 — `CREATE INDEX` / `REINDEX`, toplu kopyalama, `VACUUM` ve `ALTER TABLE` C'nin yürüttüğü gibi.** (1) **İndeks kurma**: şema satırı önce yazılıyor; anahtarlar kararlı bir sıralayıcıdan geçip C'nin toplu imleciyle (`BTREE_BULKLOAD` → `balance_nonroot(bBulk=1)`) sıralı ekleniyor; `UNIQUE` denetimi bir önceki anahtarla karşılaştırarak yapılıyor; `REINDEX` 3.54.1'in tek döngüsü. Motorun sayfaları doğrudan yazan kendi toplu yükleyicisi kaldırıldı (yapraklar %90 yerine C gibi ~%98–99 dolu). (2) **Aktarım optimizasyonu** (`INSERT INTO d SELECT * FROM s`): C'nin 31 koşulu aynı sırayla; satırlar yeniden kodlanmadan hücre hücre kopyalanıyor, indeksler toplu imleçle; satır kimliği kuralları C ile aynı. (3) **`VACUUM` / `VACUUM INTO`**: C'nin ifade sırası (tablolar, indeksler, tablo başına aktarım, kalan şema satırları, başlık değerleri, dosya kopyası). (4) **`ALTER TABLE`**: şema satırları C'nin iç içe `UPDATE` geçişleriyle yazılıyor; yeniden adlandırma `sqlite_stat1`e dokunmuyor. Ayrıca bir B-tree kuralı: az önce değiştirilen satırın bir sonraki anahtarına C imleci ilerleterek ulaşıyor ve aynı boyutlu kaydın üzerine yazıyor. İki entegrasyon testi beklentisi C'ye göre değişti (indeksli tabloda `VACUUM` örtük satır kimliklerini korur; yeniden adlandırmadan sonra `sqlite_stat1` eski adı tutar).

- **Testler**: `cargo test --workspace`: 810 geçti (18 yeni; 13'ü C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **195 → 215**; kötüleşen yok, 21 iyileşti (17 indeks/`VACUUM` ve 4 yeniden adlandırma senaryosunun hepsi L0). Kalan 30: R-Tree 20, FTS5 7, üç tekil (WITHOUT ROWID silme sırası, taşma sayfalarında önbellek bağımlı baytlar, özel kilit kipinde sayaç, `PRAGMA optimize`, rastgele satır kimliği). **Karar eşliği** (boş dosyadan; 322 betik, 6.514.115 C olayı): 321 betikte olay dizisi aynı, 320'si her yönüyle eşit ve bayt bayt aynı; `bBulk=1` dengeleme adımı C'de 13.016, motorda 13.016; aktarım koşullarının #1–#27 ve #29–#31'i eşit sayıda tetiklendi. Ayrıntı: `karar_esligi_h3.md`. **Sağlık:** `s05` (sonradan indeks) artık bayt bayt aynı, 458/458 sayfa (önce 490/458). **Farksal koşular:** yeni karışık koşu 5.000 tohum — stdout C ile aynı 4.997, her dosya iki yönde bütünlük kontrolünden geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** 99/100, 94/94, 99/100 (değişmedi). **Çökme/geri alma:** dört takım, 0 hata (yeni takım: indeks kurma, `VACUUM`, kopyalama, yeniden adlandırma; 372 betik + 3.794 kurtarma). **Bedel / kazanç:** toplam 0,993x; `W02` 7,45 → 5,64 ms (0,757x; C 8,76 ms). **Geri kazanılmayan:** 84 MB veritabanında beş toplu ifade C'nin 1,3–2,1 katı sürüyor (`VACUUM` 565 ms'ye karşı 393; kopyalama 438'e karşı 210) ve `CREATE INDEX`/`REINDEX` orada H2'den %8/%13 yavaş; H2'ye göre `VACUUM` 2,2 kat, kopyalama 6,1 kat hızlandı. **Hizalanmayan:** `sqlite_rename_quotefix` (yeniden adlandırmada çift tırnaklı dizgelerin çevrilmesi); tablo `UNIQUE` kısıtı ile `CREATE UNIQUE INDEX` ayrımı aktarım uyumluluğunda yok.
- **Toplam**: `247.08 ms` (taban `248.08 ms`, C `294.10 ms`) — geomean `Rust/C` **`0.809x`** (disk WAL `0.779x`, bellek-içi `0.925x`)
- **Tepe RSS**: `18,700 KiB` (C: `6,364 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.87`** | `7.65` | `7.58` | **`1.037x`** | `1.040x` | `9.60` | **`0.82x`** | `9,112` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.65`** | `5.61` | `7.51` | **`0.752x`** | `0.757x` | `8.65` | **`0.65x`** | `10,072` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.31`** | `28.04` | `27.96` | **`1.012x`** | `1.024x` | `23.41` | **`1.21x`** | `10,072` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.80`** | `7.68` | `7.80` | **`1.001x`** | `0.998x` | `7.43` | **`1.05x`** | `10,136` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.06`** | `4.04` | `4.28` | **`0.950x`** | `0.949x` | `16.52` | **`0.25x`** | `10,136` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.82`** | `10.77` | `10.65` | **`1.016x`** | `1.004x` | `28.26` | **`0.38x`** | `10,264` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`37.96`** | `37.72` | `38.77` | **`0.979x`** | `0.981x` | `50.12` | **`0.76x`** | `13,772` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.74`** | `9.61` | `9.38` | **`1.039x`** | `1.020x` | `8.00` | **`1.22x`** | `13,772` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.09`** | `12.06` | `11.86` | **`1.019x`** | `1.018x` | `19.01` | **`0.64x`** | `13,772` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.99`** | `10.90` | `11.15` | **`0.986x`** | `0.976x` | `10.94` | **`1.00x`** | `13,804` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.20`** | `44.90` | `46.08` | **`1.003x`** | `1.006x` | `57.43` | **`0.80x`** | `18,472` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.86`** | `23.41` | `23.89` | **`0.999x`** | `1.006x` | `24.34` | **`0.98x`** | `18,492` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.94`** | `4.65` | `4.78` | **`1.034x`** | `1.003x` | `4.01` | **`1.23x`** | `18,524` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.80`** | `36.46` | `36.30` | **`1.014x`** | `1.007x` | `26.39` | **`1.39x`** | `18,492` | `fc7490e2b80c68e2` ✓ |

---

## 05_h4_rtree — `performans-A-hizalama@b734244`

**H4 — R-Tree, C'nin `rtree.c` dosyasının portu olarak.** `crates/sqliters-ext/src/rtree.rs` yeniden yazıldı: ağaç algoritması (`ChooseLeaf`, `AdjustTree`, `SplitNode`/`splitNodeStartree`/`SortByDimension`, `float`/`double` hücre aritmetiği, silmede `removeNode` + `reinsertNodeContent`), düğüm nesneleri ve **yazma zamanı** (düğüm son başvurusu bırakılınca yazılır; ifadeler arası önbellek ve geri alma kaydı yok), sorgular (`rtreeBestIndex`, arama noktası kuyruğu — satır sırasını belirler, `SQLITE_LOCKED_VTAB`), `rtreecheck`. Motorun kendi ağaç algoritması, sınırsız düğüm önbelleği ve geri alma kaydı kaldırıldı. **Ana motor tarafında tüm sanal tablolar için C'nin kuralları**: değerler `xUpdate`e yakınlık uygulanmadan ulaşır; çok satırlı `DELETE` satır kümesiyle (artan satır kimliği); modülün iç ifadeleri üst düzey ifade gibi sayılır (`changes()`/`total_changes()`); `IN` ve `OR` planları C'nin maliyet modeliyle; sanal tablo taraması için EQP metni; sanal tablo yeniden adlandırma C'nin sırasıyla (ad tırnaklı, görünüm/tetikleyiciler yeniden yazılır). İki pager kuralı: serbest listeden alınan sayfa açık kayıt noktaları için saklanmaz; otomatik commit'te açılan `SAVEPOINT` pager kayıt noktası sayılmaz. Bir entegrasyon testi beklentisi C'ye göre değişti (yeniden adlandırılan sanal tablonun şema metninde ad tırnak içinde).

- **Testler**: `cargo test --workspace`: 832 geçti (25 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **215 → 233**; kötüleşen yok; R-Tree adlı 22 senaryonun hepsi L0. Kalan 12: FTS5 7, beş tekil. **Karar eşliği** (boş dosyadan; 130 R-Tree betiği, 715.577 karar olayı): 130/130 her yönüyle eşit ve bayt bayt aynı; tetiklenmeyen: geometri puanlı arama kuyruğu (geri çağırma yok), bozuk düğüm dalları. Ayrıntı: `karar_esligi_h4.md`. **Farksal koşular:** CLI 3.000 tohum — dosyalar bayt bayt aynı 2.418/2.418 (H3 düzeyinde 0), iki yönde `integrity_check` + `rtreecheck` tamam; API 1.000 tohum — çıktı ve dosyalar 1.000/1.000; karışık yazıcılar (C→Rust, Rust→C) 253 geçmişte son dosya C→C ile aynı. **Sayaçlar** her ifadeden sonra C ile aynı (ör. oluşturma 1/1, kimlikli ekleme toplam +3, güncelleme +5, silme +3). **Bağımsız kontrol:** 7.191 satırlık çok R-Tree'li rastgele betikte dosya, stdout ve stderr C ile aynı. **Sağlık:** s12 73/73 sayfa, 152/152 düğüm; s13 81/81; ikisi de bayt bayt aynı. **Bellek (tepe RSS):** tek işlemde 1e6 ekleme 267 MB → 61,6 MB (C 6,9 MB); modülün kendi belleği sınırlı, kalan pager'ın işlem sayfalarını bellekte tutmasından. 1e5 otomatik commit ekleme: 162,7 G → 8,87 G komut (C 5,06 G). **Çökme/geri alma:** beş takım, 0 hata. **Bedel:** `W12` 23,72 → 33,54 ms (1,414x; komut 1,284x; C 24,63 ms); `W11` 0,936x (ortak sanal tablo altyapısı ucuzladı); toplam 1,029x. Bedelin nedeni C'nin yazma zamanı: her eklemede yol düğümleri okunuyor ve gölge tablolara birkaç B-tree işlemi yapılıyor; C aynı işi 154,6 M komutla yapıyor, motor 190,4 M. **Hizalanmayan:** birleşimde sanal tablo varken birleşim sırası (C'nin yol çözücüsü; `ORDER BY`sız satır sırası ve EQP farklı), geometri/`MATCH` geri çağırmaları, geopoly. FTS5 sayaçları ve EQP `idxNum`/`idxStr` değerleri hâlâ C'ninki değil (FTS5 modülü bu adımda değişmedi).
- **Toplam**: `252.59 ms` (taban `248.08 ms`, C `294.10 ms`) — geomean `Rust/C` **`0.822x`** (disk WAL `0.777x`, bellek-içi `1.014x`)
- **Tepe RSS**: `18,652 KiB` (C: `6,364 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.68`** | `7.60` | `7.87` | **`0.976x`** | `1.015x` | `9.60` | **`0.80x`** | `9,148` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.64`** | `5.59` | `5.65` | **`0.999x`** | `0.756x` | `8.65` | **`0.65x`** | `10,108` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.17`** | `28.10` | `28.31` | **`0.995x`** | `1.019x` | `23.41` | **`1.20x`** | `10,108` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.82`** | `7.69` | `7.80` | **`1.002x`** | `0.999x` | `7.43` | **`1.05x`** | `10,172` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.12`** | `4.09` | `4.06` | **`1.014x`** | `0.963x` | `16.52` | **`0.25x`** | `10,172` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.81`** | `10.75` | `10.82` | **`0.999x`** | `1.003x` | `28.26` | **`0.38x`** | `10,300` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`37.59`** | `37.53` | `37.96` | **`0.990x`** | `0.972x` | `50.12` | **`0.75x`** | `13,808` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.71`** | `9.67` | `9.74` | **`0.997x`** | `1.017x` | `8.00` | **`1.21x`** | `13,872` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.14`** | `12.04` | `12.09` | **`1.004x`** | `1.022x` | `19.01` | **`0.64x`** | `13,872` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.22`** | `11.06` | `10.99` | **`1.021x`** | `0.996x` | `10.94` | **`1.03x`** | `13,840` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`42.39`** | `41.63` | `46.20` | **`0.918x`** | `0.923x` | `57.43` | **`0.74x`** | `18,588` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`33.46`** | `32.64` | `23.86` | **`1.403x`** | `1.410x` | `24.34` | **`1.38x`** | `18,592` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.85`** | `4.58` | `4.94` | **`0.983x`** | `0.986x` | `4.01` | **`1.21x`** | `18,624` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.98`** | `36.66` | `36.80` | **`1.005x`** | `1.012x` | `26.39` | **`1.40x`** | `18,592` | `fc7490e2b80c68e2` ✓ |

---

## 06_h5_fts5 — `performans-A-hizalama@ba3b169`

**H5 — FTS5 yazma yolu, C'nin `fts5_hash.c` / `fts5_index.c` / `fts5_storage.c` dosyalarının portu olarak.** Yeni `crates/sqliters-ext/src/fts5_index.rs` (5.017 satır): bellek içi karma tablo ve boşaltma eşikleri (`hashsize`, aynı satıra ikinci yazma, küçülen satır kimliği), segment yazıcı (yaprak bölme, `%_idx` ayırıcı anahtarları, doclist indeksi), segment kimliği ve düzey yerleşimi, otomatik birleştirme / kriz birleştirmesi / `fts5StructurePromote`, `optimize` ve `merge`, güvenli silme, mezar taşları ve `contentless_delete`. `fts5.rs` içindeki depolama katmanı C'nin ifade sırasıyla yazıyor (iç ifadeler sayılıyor); `xSync` / `xSavepoint` / `xRelease` / `xRollbackTo` kancaları `fts5_main.c` gibi. Motorun kendi yazıcısı (bekleyen işlem listesi, sıkıştırma ile boşaltma, segment kodlayıcı; `fts5.rs`den 2.336 satır) kaldırıldı. Okuma tarafı segmentleri C sırasıyla ve boşaltılmamış karma tabloyu en yeni kaynak olarak okuyor. `xBestIndex` (`fts5` ve `fts5vocab`) C'nin portu: EQP'deki `idxNum`/`idxStr` (`0:M2`, `0:=`, …) ve maliyetler C ile aynı. **Ana motor tarafında:** sanal tablo kayıt noktası kancaları pager ifade kayıt noktasından önce ve C'nin numaralandırmasıyla; `RELEASE` sırası C gibi; ifade başında `OP_VBegin` (0 satırlık DML ve tetikleyicinin yazdığı sanal tablolar dahil); commit anındaki `xSync`ten sonra `changes()` korunuyor; `SQLITE_CorruptRdOnly` (işlem içinde `SQLITE_CORRUPT` alan ifadeden sonra yazmalar reddedilir, `COMMIT` geri alır); modülün bulduğu bozulma `SQLITE_CORRUPT_VTAB` (267). **Pager:** alt günlük kuralları C'ye getirildi (`subjRequiresPage`, `bTruncateOnRelease`, ana günlüğe yeni giren sayfa alt günlüğe yazılmaz, geri almadan sonra iç kayıtlar durur). Test beklentisi değişmedi.

- **Testler**: `cargo test --workspace`: 853 geçti (21 yeni: 13'ü C'nin yazdığı dosyayla eşitlik, 8'i satır/sayaç), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **233 → 240**; kötüleşen yok; FTS5 senaryolarının 7'si de L0. Kalan 5 (hiçbiri FTS5 değil): WITHOUT ROWID silme sırası (s10), taşma sayfalarında önbellek bağımlı baytlar (s11), özel kilit kipinde değişiklik sayacı, `PRAGMA optimize`, 2^63−1 sonrası rastgele satır kimliği. **Karar eşliği** (boş dosyadan; 131 FTS5 betiği, her iki tarafta 4,10 M karar olayı): 128 betik her yönüyle eşit, 131/131 dosya bayt bayt aynı. Eşit olmayan üç iz: `xRename` içindeki iç `ALTER` ifadelerinin kayıt noktası kancaları (yalnız izde), ve `PRAGMA integrity_check` altında C'nin fazladan bir R-Tree taraması (2 betik, FTS5 dışı). Ayrıntı: `karar_esligi_h5.md`. **Farksal koşular:** CLI 3.000 tohum — saf Rust dosyası C ile bayt bayt aynı 2.939, karışık yazıcılar 2.959; aynı olmayan 55 tohumun 54'ü pager önbellek taşması sınıfı (C `cache_size=1000000` ile koşunca hepsi bayt bayt aynı), kalanlar C'nin kendi bozduğu indeksler (34 tohum; 27'sinde Rust dosyası C'ninkiyle aynı). API 1.000 tohum (hazır ifadeler, 14 tablo yapılandırması): dosyalar 996/1000 bayt bayt aynı, çıktılar 943/1000. **Bağımsız kontrol:** beş yapılandırmada (varsayılan, porter, trigram, `detail=column`, `detail=none`) Rust'ın yazdığı dosya C ile bayt bayt aynı (H4: hiçbiri); sorgular dört yönde aynı; "C dosyası + Rust devam + optimize" = "C devam + optimize"; sayaçlar ve EQP C ile aynı. **Önceki kapılar:** H3 izleri 322/322 (önce 321), H1 95/95, R-Tree 130/130; karışık koşuda ana dosya bayt bayt aynı 1.730/2.000 (önce 1.531; FTS5 194/194, önce 0/194); DML farksalı 377/400 (değişmedi); kurtarma 94/94 + 99/100 (değişmedi). **Çökme/geri alma:** FTS5 takımı dahil 0 hata. **CLI yükleri** (C / H4 / H5, saniye): 4.000 otomatik commit ekleme 0,72 / 9,23 / 1,06 (dosya = C); tek işlemde 1e5 belge 1,19 / 5,24 / 2,63 (tepe bellek 9 / 1.185 / 59 MiB); 1e5 otomatik commit 17,9 / >300 / 28,8; 20.000 belgede 2.000 `UPDATE` 0,30 / 39,7 / 0,63. **Bedel / kazanç:** toplam 0,985x; `W11` 42,99 → 40,34 ms (0,938x; C 57,02 ms); `W12` komut sayısı 1,035x (ifade başına sanal tablo işi). **Hizalanmayan:** `fts5_locale()`; sorgu tarafı okuyucu port değil (C'nin kendi bozduğu indekslerde hata noktası farklı); `LIMIT` ötesindeki satırlarda `snippet()`/`bm25()` hatası (motor sanal tablo taramasını önce topluyor); pager önbellek taşması baytları (büyük işlemler). **Bulunan ayrı sorun:** WITHOUT ROWID + `page_size=65536` içeren bir betik ~2 dakika sürüyor (C 0,016 s; H4'te de var).
- **Toplam**: `249.29 ms` (taban `248.08 ms`, C `294.10 ms`) — geomean `Rust/C` **`0.812x`** (disk WAL `0.769x`, bellek-içi `0.991x`)
- **Tepe RSS**: `17,892 KiB` (C: `6,364 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.63`** | `7.58` | `7.68` | **`0.993x`** | `1.008x` | `9.60` | **`0.79x`** | `9,288` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.63`** | `5.62` | `5.64` | **`0.998x`** | `0.755x` | `8.65` | **`0.65x`** | `10,248` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.75`** | `27.57` | `28.17` | **`0.985x`** | `1.004x` | `23.41` | **`1.19x`** | `10,248` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.72`** | `7.61` | `7.82` | **`0.988x`** | `0.987x` | `7.43` | **`1.04x`** | `10,312` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.09`** | `4.08` | `4.12` | **`0.993x`** | `0.956x` | `16.52` | **`0.25x`** | `10,312` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.62` | `10.81` | **`0.984x`** | `0.986x` | `28.26` | **`0.38x`** | `10,440` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`37.78`** | `37.63` | `37.59` | **`1.005x`** | `0.976x` | `50.12` | **`0.75x`** | `14,012` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.65`** | `9.57` | `9.71` | **`0.994x`** | `1.011x` | `8.00` | **`1.21x`** | `14,076` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.11`** | `12.03` | `12.14` | **`0.998x`** | `1.020x` | `19.01` | **`0.64x`** | `14,076` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.98`** | `10.96` | `11.22` | **`0.978x`** | `0.975x` | `10.94` | **`1.00x`** | `14,044` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`40.40`** | `39.99` | `42.39` | **`0.953x`** | `0.880x` | `57.43` | **`0.70x`** | `17,856` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`33.57`** | `33.09` | `33.46` | **`1.003x`** | `1.415x` | `24.34` | **`1.38x`** | `17,876` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.62`** | `4.47` | `4.85` | **`0.953x`** | `0.939x` | `4.01` | **`1.15x`** | `17,908` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.72`** | `36.19` | `36.98` | **`0.993x`** | `1.005x` | `26.39` | **`1.39x`** | `17,876` | `fc7490e2b80c68e2` ✓ |

---

## 07_h6_onbellek_tasmasi — `performans-A-hizalama@27eacad`

**H6 (kısmi) — sayfa önbelleği C'nin sınırı, kirli sayfa taşması ve geri dönüşümüyle.** Yeni `crates/sqliters-pager/src/pcache.rs` (1.177 satır) C'nin `pcache.c` / `pcache1.c` defter tutmasını taşıyor: `cache_size`tan sayfa sayısı (`nMax`, `n90pct`, `mxPinned`; 4096 baytlık sayfada varsayılan 483 sayfa), sayfa başına başvuru sayısı, kirli liste sırası, `pSynced`, `NEED_SYNC` / `DONT_WRITE`, temiz sayfaların LRU ile geri dönüşümü. Önbellek doluyken yeni sayfa istenince `pagerStress` gibi bir kirli sayfa işlem ortasında yazılıyor: geri alma günlüğü kiplerinde `syncJournal` (günlük başlığındaki sihirli sayı ve kayıt sayısı o ana kadar sıfır; her eşitlemede yeni başlık bölümü) ve ilk taşmada EXCLUSIVE kilit; WAL kipinde alt günlük + çerçeve ekleme. İfade sınırındaki eski taşma yaklaşımı ve eski önbellek kaldırıldı. Yeni `PRAGMA cache_spill` ve `PRAGMA shrink_memory`; `PRAGMA cache_size` `sqlite3InitOne` kuralıyla. **C'nin hangi sayfayı yazacağı o anda hangi sayfalara başvuru tutulduğuna bağlı.** Yeni `crates/sqliters-btree/src/pins.rs` bu başvuru zaman çizelgesini ekleme, dengeleme, sayfa tahsisi/serbest bırakma, tablo boşaltma, taşma zinciri ve taramalar için üretiyor. **Henüz modellenmeyen:** `UPDATE` / `DELETE … WHERE`, satır aramaları, dolu tabloda `CREATE INDEX`, `VACUUM`, WITHOUT ROWID, kayıt noktalı betikler, otomatik vakum taşıması ve FTS5 / R-Tree iç ifadeleri — buralarda yazılan sayfa C'ninkinden farklı olabiliyor. İki test beklentisi C'ye göre değişti (eşitlenmemiş günlük sıfır baytla başlar ve "sıcak" sayılmaz; C ikilisiyle doğrulandı).

- **Testler**: `cargo test --workspace`: 864 geçti (11 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Karar eşliği** (145 betik, 15 aile; C 5,14 M olay): karar dizisi 63 betikte eşit — ekleme ve tablo boşaltma ailelerindeki 60 betiğin hepsi; `UPDATE`, `DELETE`, sorgu, taşma, kayıt noktası, otomatik vakum, DDL ve WITHOUT ROWID ailelerinde 0. Son dosya bayt bayt aynı 140/145 (H5: 124), stdout/stderr 145/145. Hiç gözlenmeyen beş sonuç ve sınanmayan alanlar `karar_esligi_h6.md` içinde "doğrulanmadı" olarak listeli. **Taşma sınıfı kapısı karşılanmadı:** H5'te yalnızca boş alan baytlarıyla ayrışan 54 tohumun 8'i bayt bayt aynı oldu; yüklerde C'den farklı sayfa sayısı `ins1e5_tx` 404 → 46, `upd2000_tx` 395 → 11, `del2000_tx` 584 → 20. **Yeni taşma fuzz'ı** (400 tohum; günlük kipi × sayfa boyutu × önbellek × taşma eşiği): her yönüyle aynı 375 (H5: 0); dosya baytı farkı 17 → 1; işlem içi WAL boyutu farkı 72 → 11; `wal_checkpoint` sayıları 11 → 1. **Ayrışma bulucu:** 240/245, değişmedi, kötüleşen yok (`s11` taşma sınıfı olarak doğrulandı; farklı sayfa 89 → 6). **Bağımsız kontrol:** 60.000 satırlık ekleme işlemlerinde işlem içi veritabanı, günlük ve WAL boyutları ile `wal_checkpoint` çıktısı C ile aynı (H5: WAL kipinde `0|4354|4354`, C `0|4353|4353`); `UPDATE`/`DELETE` işlemlerinde son dosyalar aynı ama işlem içi günlük/WAL boyutu C'den farklı. **Çökme/geri alma:** yeni taşma takımı 432 betik, 13.696 kurtarma, 0 hata; önceki altı takım 0 hata; ayrıca öldürülen 192 yazıcı görüntüsü iki motorla da doğru kurtarıldı. **Süreçler arası:** 16/16 eşleşmede C↔C ile aynı BUSY/kilit dökümü. **Önceki kapılar:** DML farksalı 377/400, kurtarma 94/94 + 99/100, H1/H3/H4 izleri değişmedi. **Bellek (tepe RSS, MiB; C / H5 / H6):** 1e6 R-Tree eklemesi 6,3 / 60,1 / 10,2; `ins1e5_tx` 8,4 / 59,2 / 17,4; `upd2000_tx` 7,2 / 29,6 / 20,3. **Bedel:** toplam 1,025x; `W01` 1,118x, `W12` 1,096x, `W10` 1,049x, `W13` 0,955x; komut sayıları +1,6 … +5,7 % (`W13` −2,2 %). 1e6 R-Tree eklemesi 14,3 → 21,9 s (C 14,8 s). **H5'e göre kötüleşen:** (1) küçük önbellekli bir WAL `UPDATE` betiğinde `PRAGMA wal_checkpoint` `0|225|225` (C ve H5 `0|224|224`); (2) 65536 baytlık sayfada `DELETE … WHERE id IN (SELECT … LIMIT 7)` 57 s → ~295 s (C 0,017 s). **Bağlanmayan:** `sqlite3_db_status` önbellek sayaçları ve `.stats`; sonradan eklenen veritabanlarına `cache_spill`; taşma anında meşgul işleyicisi; alt günlüğün geçici dosyaya taşması.
- **Toplam**: `255.00 ms` (taban `248.08 ms`, C `294.10 ms`) — geomean `Rust/C` **`0.831x`** (disk WAL `0.782x`, bellek-içi `1.036x`)
- **Tepe RSS**: `17,680 KiB` (C: `6,364 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`8.42`** | `8.33` | `7.63` | **`1.104x`** | `1.113x` | `9.60` | **`0.88x`** | `9,364` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.64`** | `5.57` | `5.63` | **`1.001x`** | `0.755x` | `8.65` | **`0.65x`** | `10,332` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.06`** | `27.91` | `27.75` | **`1.011x`** | `1.015x` | `23.41` | **`1.20x`** | `10,332` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`8.06`** | `7.83` | `7.72` | **`1.044x`** | `1.031x` | `7.43` | **`1.09x`** | `10,396` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.28` | `4.09` | **`1.047x`** | `1.001x` | `16.52` | **`0.26x`** | `10,396` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.53`** | `10.52` | `10.63` | **`0.990x`** | `0.977x` | `28.26` | **`0.37x`** | `10,588` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.28`** | `37.77` | `37.78` | **`1.013x`** | `0.989x` | `50.12` | **`0.76x`** | `12,908` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.91`** | `9.61` | `9.65` | **`1.027x`** | `1.038x` | `8.00` | **`1.24x`** | `12,908` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.81`** | `11.76` | `12.11` | **`0.975x`** | `0.994x` | `19.01` | **`0.62x`** | `12,908` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.62`** | `11.57` | `10.98` | **`1.059x`** | `1.032x` | `10.94` | **`1.06x`** | `13,092` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`41.96`** | `41.39` | `40.40` | **`1.039x`** | `0.914x` | `57.43` | **`0.73x`** | `17,580` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`34.89`** | `33.51` | `33.57` | **`1.039x`** | `1.471x` | `24.34` | **`1.43x`** | `17,604` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.52`** | `4.36` | `4.62` | **`0.978x`** | `0.919x` | `4.01` | **`1.13x`** | `17,636` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`37.01`** | `36.51` | `36.72` | **`1.008x`** | `1.013x` | `26.39` | **`1.40x`** | `17,604` | `fc7490e2b80c68e2` ✓ |
