# `performans-A-hizalama` — C'ye Hizalama Adımları (Bedelli) Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-14T08:14:36+03:00`
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
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`294.77`** | `—` | **`1.000x`** | **`1.000x`** | `6,372` | `—` |
| **00_performans_A_ucu** | `performans-A@225f5c2` | `7,299.0` | `6,157.8` | **`248.18`** | `1.000x` | **`0.820x`** | `0.842x` | `18,180` | 14/14 ✓ |
| **01_h1_silmede_dengeleme** | `performans-A-hizalama@034af4f` | `7,346.9` | `6,193.5` | **`248.30`** | `1.001x` | **`0.821x`** | `0.842x` | `18,420` | 14/14 ✓ |
| **02_h1b_dml_stratejisi** | `performans-A-hizalama@ebc3fde` | `7,461.8` | `6,294.8` | **`249.59`** | `1.006x` | **`0.825x`** | `0.847x` | `18,376` | 14/14 ✓ |
| **03_h2_eklemede_dengeleme** | `performans-A-hizalama@0c65dbe` | `7,376.3` | `6,223.2` | **`249.74`** | `1.002x` | **`0.822x`** | `0.847x` | `18,184` | 14/14 ✓ |
| **04_h3_toplu_kurma** | `performans-A-hizalama@fd7364d` | `7,363.6` | `6,217.5` | **`248.52`** | `0.987x` | **`0.809x`** | `0.843x` | `18,628` | 14/14 ✓ |
| **05_h4_rtree** | `performans-A-hizalama@b734244` | `7,420.3` | `6,267.2` | **`254.91`** | `1.009x` | **`0.827x`** | `0.865x` | `18,704` | 14/14 ✓ |
| **06_h5_fts5** | `performans-A-hizalama@ba3b169` | `7,516.1` | `6,347.9` | **`249.97`** | `0.991x` | **`0.812x`** | `0.848x` | `17,880` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_A_ucu | 01_h1_silmede_dengeleme | 02_h1b_dml_stratejisi | 03_h2_eklemede_dengeleme | 04_h3_toplu_kurma | 05_h4_rtree | 06_h5_fts5 |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.65` | `7.55` (**0.78x**) | `7.55` (**0.78x**) | `7.49` (**0.78x**) | `7.57` (**0.78x**) | `7.89` (**0.82x**) | `8.12` (**0.84x**) | `7.66` (**0.79x**) |
| `W02_create_indexes` | `8.65` | `7.45` (**0.86x**) | `7.52` (**0.87x**) | `7.56` (**0.87x**) | `7.54` (**0.87x**) | `5.69` (**0.66x**) | `5.71` (**0.66x**) | `5.70` (**0.66x**) |
| `W03_pk_point_select` | `23.64` | `28.07` (**1.19x**) | `28.34` (**1.20x**) | `28.20` (**1.19x**) | `28.27` (**1.20x**) | `28.42` (**1.20x**) | `28.15` (**1.19x**) | `27.88` (**1.18x**) |
| `W04_secondary_idx_range` | `7.50` | `7.81` (**1.04x**) | `7.79` (**1.04x**) | `7.83` (**1.04x**) | `7.77` (**1.04x**) | `7.87` (**1.05x**) | `7.84` (**1.05x**) | `7.69` (**1.03x**) |
| `W05_full_scan_filter_agg` | `16.59` | `4.28` (**0.26x**) | `4.37` (**0.26x**) | `4.28` (**0.26x**) | `4.28` (**0.26x**) | `4.07` (**0.24x**) | `4.11` (**0.25x**) | `4.10` (**0.25x**) |
| `W06_join_group_order` | `28.29` | `10.79` (**0.38x**) | `10.67` (**0.38x**) | `10.95` (**0.39x**) | `10.62` (**0.38x**) | `10.79` (**0.38x**) | `10.79` (**0.38x**) | `10.62` (**0.38x**) |
| `W07_window_functions` | `50.16` | `38.75` (**0.77x**) | `38.39` (**0.77x**) | `38.63` (**0.77x**) | `38.72` (**0.77x**) | `37.95` (**0.76x**) | `37.71` (**0.75x**) | `38.30` (**0.76x**) |
| `W08_indexed_update_delete` | `8.05` | `9.56` (**1.19x**) | `9.90` (**1.23x**) | `9.79` (**1.22x**) | `9.45` (**1.17x**) | `9.84` (**1.22x**) | `9.94` (**1.24x**) | `9.74` (**1.21x**) |
| `W09_like_text_scan` | `18.99` | `11.86` (**0.62x**) | `12.04` (**0.63x**) | `11.84` (**0.62x**) | `11.90` (**0.63x**) | `12.24` (**0.64x**) | `12.11` (**0.64x**) | `12.17` (**0.64x**) |
| `W10_jsonb_extract_agg` | `10.95` | `11.24` (**1.03x**) | `11.13` (**1.02x**) | `11.20` (**1.02x**) | `11.13` (**1.02x**) | `11.04` (**1.01x**) | `11.28` (**1.03x**) | `11.00` (**1.00x**) |
| `W11_fts5_index_and_bm25` | `57.28` | `45.65` (**0.80x**) | `45.45` (**0.79x**) | `45.79` (**0.80x**) | `46.91` (**0.82x**) | `46.65` (**0.81x**) | `42.76` (**0.75x**) | `40.20` (**0.70x**) |
| `W12_rtree_insert_and_bbox` | `24.36` | `23.75` (**0.97x**) | `23.73` (**0.97x**) | `24.65` (**1.01x**) | `24.08` (**0.99x**) | `24.19` (**0.99x**) | `33.59` (**1.38x**) | `33.18` (**1.36x**) |
| `W13_autocommit_wal_txns` | `4.13` | `4.95` (**1.20x**) | `4.78` (**1.16x**) | `4.95` (**1.20x**) | `5.01` (**1.21x**) | `4.99` (**1.21x**) | `4.94` (**1.20x**) | `4.80` (**1.16x**) |
| `W14_cli_sql_script` | `26.55` | `36.46` (**1.37x**) | `36.64` (**1.38x**) | `36.43` (**1.37x**) | `36.51` (**1.38x**) | `36.89` (**1.39x**) | `37.86` (**1.43x**) | `36.93` (**1.39x**) |
| **Toplam** | **`294.77`** | **`248.18`** (**0.84x**) | **`248.30`** (**0.84x**) | **`249.59`** (**0.85x**) | **`249.74`** (**0.85x**) | **`248.52`** (**0.84x**) | **`254.91`** (**0.86x**) | **`249.97`** (**0.85x**) |

---

## 00_performans_A_ucu — `performans-A@225f5c2`

**Baz.** `performans-A` dalının bu dal ayrılırken ucu (adım 5j). Ayrışma bulucu, 234 senaryo, son dosya katmanı: L0 56, L2 43, L3 3, L4 58, L5 59, L6 15.

- **Testler**: `cargo test --workspace`: 757 geçti, bilinen 4 başarısız, 337 yok sayıldı.
- **Toplam**: `248.18 ms` (taban `248.18 ms`, C `294.77 ms`) — geomean `Rust/C` **`0.820x`** (disk WAL `0.793x`, bellek-içi `0.928x`)
- **Tepe RSS**: `18,180 KiB` (C: `6,372 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.55`** | `7.53` | `1.000x` | `9.65` | **`0.78x`** | `9,032` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.45`** | `7.42` | `1.000x` | `8.65` | **`0.86x`** | `9,872` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.07`** | `27.78` | `1.000x` | `23.64` | **`1.19x`** | `9,872` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.81`** | `7.71` | `1.000x` | `7.50` | **`1.04x`** | `9,936` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `1.000x` | `16.59` | **`0.26x`** | `9,936` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.79`** | `10.76` | `1.000x` | `28.29` | **`0.38x`** | `10,064` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.75`** | `38.43` | `1.000x` | `50.16` | **`0.77x`** | `12,552` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.56`** | `9.48` | `1.000x` | `8.05` | **`1.19x`** | `12,680` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.86`** | `11.80` | `1.000x` | `18.99` | **`0.62x`** | `12,680` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.24`** | `11.18` | `1.000x` | `10.95` | **`1.03x`** | `12,864` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.65`** | `44.85` | `1.000x` | `57.28` | **`0.80x`** | `17,988` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.75`** | `23.22` | `1.000x` | `24.36` | **`0.97x`** | `18,036` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.95`** | `4.74` | `1.000x` | `4.13` | **`1.20x`** | `18,068` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.46`** | `36.25` | `1.000x` | `26.55` | **`1.37x`** | `18,036` | `fc7490e2b80c68e2` ✓ |

---

## 01_h1_silmede_dengeleme — `performans-A-hizalama@034af4f`

**H1 — silmede C gibi yeniden dengeleme.** C, bir silmeden sonra sayfanın boş alanı kullanılabilir alanın üçte ikisini aşarsa `balance()` çağırır ve `balance_nonroot()` hücreleri en çok üç kardeş arasında yeniden dağıtır (gerekirse bir sayfayı serbest bırakır, kökte bir seviye düşürür); iç sayfadaki bir hücre silinince yerine öncül yaprak hücresi konur. Motor yalnızca yaprak tamamen boşaldığında davranıyordu; iç indeks hücresi silmede bazı durumlarda tüm indeksi yeniden kuruyordu. Yeni `sqliters-btree/src/balance.rs`, C'nin `balance` / `balance_nonroot` / `balance_deeper` / `balance_quick`, `insertCell` / `dropCell` / `allocateSpace` / `defragmentPage`, `editPage` / `rebuildPage` işlevlerinin portudur; tüm silme yolları (genel, konumlu, ham hızlı yollar, indeks) buna bağlandı. Hızlı yollar ham kaldı: üçte iki testi O(1) bir sınırla başlıyor. Ekleme tarafı bu adımda değişmedi (H2 aynı kodu etkinleştirecek). Motor düzeyinde bir düzeltme de gerekti: satır kimliği çakışmasında `REPLACE` artık C gibi yalnızca indeks girdilerini silip satırın üzerine yazıyor.

- **Testler**: `cargo test --workspace`: 764 geçti (7 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Karar eşliği** (iki motor aynı başlangıç dosyasından; 95 senaryo, 762.748 C olayı): 93 senaryoda her karar, SQL çıktısı, bütünlük ve sayfa sayfa şekil C ile aynı; kalan ikisi bu adımdan önce de var olan motor düzeyi farklar (indeks taramalı silmede ağaçlar arası yazma sırası; tetikleyici içi `DELETE`in satır ziyaret sırası). Hiç tetiklenmeyen kararlar: `balance_quick` ve `bBulk` dalları (yalnızca eklemede), bozuk girdi dalları. Ayrıntı: `karar_esligi_h1.md`. **Ayrışma bulucu** (245 senaryo, son derleme): kötüleşen yok, 24 iyileşti (L5 59 → 39, L2 44 → 55). **Sayfa sayıları (motor/C):** karışık 4096: 296 → 251 / 249; 512: 2456 → 2214 / 2173; 1024 + auto_vacuum: 1182 → 986 / 972; WAL: 281 → 244 / 244. Kalan fark ekleme tarafı bölünmelerden (H2). **Farksal koşu:** 4.800 tohum, çıktı C ile ve dal noktasıyla aynı, her dosya C `integrity_check` geçiyor. **Çökme/geri alma:** 288 betik + commit sırasında öldürülen 2.660 kurtarma, 0 hata. **Kurtarma eşliği (kapı tam karşılanmadı):** otomatik commit'te aynı şekilli 93 dosyanın 93'ünde C `.recover` çıktısı aynı; açık işlem içinde 99'un 84'ünde. Neden bu adımdan önce de var: motor açık işlemdeki her genel-yol ifadesi için ifade kayıt noktası açıyor (C yalnızca çok satırlı ve iptal olabilecek ifadelerde), bu yüzden serbest kalan sayfanın yazımı düşürülemiyor. **Bedel:** `W08` süre +%5,0 (ajanın koşusunda +%3,3), komut +%0,75; diğer iş yüklerinde komut sayısı aynı. Kalan maliyet: C sayfa başına boş alanı önbellekte tutuyor (`nFree`), port gerektiğinde yeniden hesaplıyor.
- **Toplam**: `248.30 ms` (taban `248.18 ms`, C `294.77 ms`) — geomean `Rust/C` **`0.821x`** (disk WAL `0.795x`, bellek-içi `0.923x`)
- **Tepe RSS**: `18,420 KiB` (C: `6,372 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.55`** | `7.48` | `7.55` | **`1.000x`** | `1.000x` | `9.65` | **`0.78x`** | `9,104` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.52`** | `7.44` | `7.45` | **`1.009x`** | `1.009x` | `8.65` | **`0.87x`** | `10,000` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.34`** | `27.93` | `28.07` | **`1.010x`** | `1.010x` | `23.64` | **`1.20x`** | `10,000` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.79`** | `7.66` | `7.81` | **`0.997x`** | `0.997x` | `7.50` | **`1.04x`** | `10,064` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.37`** | `4.35` | `4.28` | **`1.020x`** | `1.020x` | `16.59` | **`0.26x`** | `10,064` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.67`** | `10.63` | `10.79` | **`0.989x`** | `0.989x` | `28.29` | **`0.38x`** | `10,196` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.39`** | `38.22` | `38.75` | **`0.991x`** | `0.991x` | `50.16` | **`0.77x`** | `12,688` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.90`** | `9.79` | `9.56` | **`1.035x`** | `1.035x` | `8.05` | **`1.23x`** | `12,688` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.04`** | `12.00` | `11.86` | **`1.015x`** | `1.015x` | `18.99` | **`0.63x`** | `12,688` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.13`** | `11.08` | `11.24` | **`0.990x`** | `0.990x` | `10.95` | **`1.02x`** | `12,868` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.45`** | `44.97` | `45.65` | **`0.996x`** | `0.996x` | `57.28` | **`0.79x`** | `18,132` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.73`** | `23.63` | `23.75` | **`0.999x`** | `0.999x` | `24.36` | **`0.97x`** | `18,160` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.78`** | `4.65` | `4.95` | **`0.965x`** | `0.965x` | `4.13` | **`1.16x`** | `18,192` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.64`** | `36.33` | `36.46` | **`1.005x`** | `1.005x` | `26.55` | **`1.38x`** | `18,160` | `fc7490e2b80c68e2` ✓ |

---

## 02_h1b_dml_stratejisi — `performans-A-hizalama@ebc3fde`

**H1b — DML ifadelerinin C'nin kod üreticisi gibi yürütülmesi.** (A) **Geçiş stratejisi ve satır ziyaret sırası**: `DELETE` ve `UPDATE` için C'nin kararları port edildi (tek geçiş / satır kümesi / geçici indeks; `bComplex`, `chngKey`, `hasFK`, kısaltma optimizasyonu koşulları); iki geçişte satırlar C gibi satır kimliği/birincil anahtar sırasıyla işleniyor. Tetikleyicili tabloda indeks üzerinden `DELETE` artık C ile aynı sonucu veriyor (önce sonuç farklıydı). `AUTOINCREMENT` ifade başına sayaç kullanıyor; `last_insert_rowid` ve `total_changes` başarısız ifadede C gibi geri alınmıyor; tek satırlık `INSERT`te eksik üst satır hemen hata veriyor. (B) **Kısıt kontrol sırası ve yazma sırası**: NOT NULL → CHECK → satır kimliği → UNIQUE indeksler (C'nin indeks sırasıyla; önce ters sıradaydı); `STRICT` tür kontrolü C'nin konumunda; `REPLACE` çakışan indeks girdisini en son siliyor; üretilmiş sütunlar bağımlılık sırasıyla hesaplanıyor (ileri başvuruda yanlış indeks girdisi yazılıyor ve C `integrity_check` düşüyordu). (C) **İfade günlüğü** yalnızca C'nin açtığı durumda açılıyor (`isMultiWrite && mayAbort`); başarısız ifadenin neyi geri aldığı `sqlite3VdbeHalt` kurallarına bağlandı. Bir entegrasyon testi beklentisi C'ye göre değişti (başarısız commit'ten sonra `last_insert_rowid`).

- **Testler**: `cargo test --workspace`: 781 geçti (17 yeni), bilinen 4 başarısız, 337 yok sayıldı. **DML farksalı** (rastgele şema, tetikleyici, yabancı anahtar, çakışma yan tümceleri; CLI + API; her ifadeden sonra `changes`/`total_changes`/`last_insert_rowid`): ajanın 3.000 tohumunda C ile birebir aynı 2.839; bağımsız 400 yeni tohumda (yalnız CLI) **377** — H1 ve dal noktası 134. Dal noktasında görülen 2 C `integrity_check` hatası kalmadı. Kalan farklar (örnekle sınıflandı): SELECT planlayıcısından gelen satır sırası (görünümler, WITHOUT ROWID indeks seçimi), DDL hata metinleri, iki yabancı anahtar ayrıntısı, `INSERT…SELECT`in eş-yordam durumu. **Karar izi** (600 tohum, 6.487 ifade): günlük açma 0 uyuşmazlık, durdurma sonucu 0, strateji 28, günlük bayrakları 9; B-tree yazma dizisi 6.184 eşit / 303 farklı. **Kurtarma eşliği:** açık işlem içinde 99/100 aynı (H1: 84/99); kalan durum serbest sayfa yolu farkı. **H1 karar izi:** 95/95 tam eşit (H1: 93). **Ayrışma bulucu:** dağılım aynı, kötüleşen yok. **Çökme/geri alma:** yeni ifade-iptali takımı 84 betik + 432 kurtarma, diğer iki takım hızlı kipte, 0 hata. **Bedel:** toplam 1,007x; komut sayıları 0,999–1,001, yalnız `W12` +%0,9 ve `W11` +%0,3 (ifade başına günlük politikası çağrısı). `W06` +%3,8 süre aynı komut sayısıyla (kod yerleşimi). **Hizalanmayan:** WITHOUT ROWID `UPDATE` tüm indeks girdilerini siliyor/yeniden ekliyor (C yalnız değişenleri); UPSERT'te silme sırası; C'nin eş-yordamlı `INSERT…SELECT`i; DDL/ANALYZE/PRAGMA ifade günlüğü bayrakları; 9 bayrak uyuşmazlığının ~5'inde C'nin günlük açtığı yerde motor açmıyor (farksalda gözlenmedi, dışlanmadı).
- **Toplam**: `249.59 ms` (taban `248.18 ms`, C `294.77 ms`) — geomean `Rust/C` **`0.825x`** (disk WAL `0.796x`, bellek-içi `0.939x`)
- **Tepe RSS**: `18,376 KiB` (C: `6,372 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.49`** | `7.35` | `7.55` | **`0.992x`** | `0.992x` | `9.65` | **`0.78x`** | `9,328` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.56`** | `7.46` | `7.52` | **`1.006x`** | `1.015x` | `8.65` | **`0.87x`** | `10,224` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.20`** | `27.56` | `28.34` | **`0.995x`** | `1.005x` | `23.64` | **`1.19x`** | `10,224` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.83`** | `7.70` | `7.79` | **`1.005x`** | `1.002x` | `7.50` | **`1.04x`** | `10,288` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `4.37` | **`0.980x`** | `1.000x` | `16.59` | **`0.26x`** | `10,288` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.95`** | `10.91` | `10.67` | **`1.025x`** | `1.014x` | `28.29` | **`0.39x`** | `10,420` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.63`** | `38.47` | `38.39` | **`1.006x`** | `0.997x` | `50.16` | **`0.77x`** | `12,912` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.79`** | `9.71` | `9.90` | **`0.989x`** | `1.023x` | `8.05` | **`1.22x`** | `12,976` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.84`** | `11.79` | `12.04` | **`0.983x`** | `0.998x` | `18.99` | **`0.62x`** | `12,976` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.20`** | `11.15` | `11.13` | **`1.006x`** | `0.996x` | `10.95` | **`1.02x`** | `13,160` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.79`** | `45.28` | `45.45` | **`1.007x`** | `1.003x` | `57.28` | **`0.80x`** | `18,440` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.65`** | `23.41` | `23.73` | **`1.039x`** | `1.038x` | `24.36` | **`1.01x`** | `18,488` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.95`** | `4.66` | `4.78` | **`1.037x`** | `1.001x` | `4.13` | **`1.20x`** | `16,332` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.43`** | `36.06` | `36.64` | **`0.994x`** | `0.999x` | `26.55` | **`1.37x`** | `16,300` | `fc7490e2b80c68e2` ✓ |

---

## 03_h2_eklemede_dengeleme — `performans-A-hizalama@0c65dbe`

**H2 — eklemede sayfa bölünmesi, hücre yerleşimi ve sayfa tahsisi C gibi.** Her B-tree eklemesi ve yerinde satır değiştirme artık C'nin `sqlite3BtreeInsert` yolunu izliyor (yeni `sqliters-btree/src/insert.rs`): aynı boyutta üzerine yazma (`btreeOverwriteCell`), `fillInCell` (yerel/taşma bölümü, taşma zincirinin tahsis sırası), var olan hücrenin değiştirilmesi, `insertCell` → `allocateSpace` (serbest bloktan ilk uyan, boşluk ya da birleştirme) ve sayfa taşınca H1'de port edilen `balance()` (`balance_quick`, üç kardeş arası dağıtım, `balance_deeper`). Hızlı yollar ham kaldı: boşluğa sığan ekleme C'de de yalnızca `insertCell`dir. Motorun kendi bölünme ve yeniden paketleme kodu (yaklaşık 3.500 satır) kaldırıldı. C'nin bıraktığı baytlar artık temizlenmiyor (yeni kök yalnız başlık; serbest listeden alınan sayfa yalnız C'nin sıfırladığı durumda sıfırlanıyor). Motor düzeyinde üç kural C'ye getirildi: WITHOUT ROWID `UPDATE` yalnız değişen ikincil indeksleri yazıyor; indekssiz tabloda `REPLACE` ve eski satırı okumayan iki geçişli `UPDATE` C'nin hücre değiştirme yolunu kullanıyor; ikincil indeksi/tetikleyicisi/yabancı anahtarı olmayan WITHOUT ROWID tabloda `REPLACE` çakışan satırı silmiyor (bir entegrasyon testi beklentisi buna göre değişti).

- **Testler**: `cargo test --workspace`: 792 geçti (11 yeni; 10'u C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile **bayt bayt aynı 58 → 195**; kötüleşen yok, 139 iyileşti. Kalan 50'yi engelleyenler: indeks toplu kurma/`VACUUM` (17, H3), `ALTER TABLE` şema satırı dizisi (4), R-Tree (18), FTS5 (7), dört tekil. **Karar eşliği** (iki motor boş dosyadan; 180 betik, 3.092.603 C olayı): 176 betikte olay dizisi, SQL çıktısı, bütünlük ve şekil aynı, 175'i bayt bayt aynı; uyuşmayan 4 betik H3'ün konusu. `balance_quick` 14.520 kez tetiklendi (H1'de 0). Tetiklenmeyen: `bBulk=1` dalları SQL üzerinden (bir alt düzeyde C'nin `CREATE INDEX` şekliyle doğrulandı), `BTREE_PREFORMAT`. Ayrıntı: `karar_esligi_h2.md`. **Sağlık senaryoları:** s01–s04 ve s06–s09 C ile bayt bayt aynı; sayfa sayıları s03, s04, s06–s11'de C ile eşit. **`secure_delete`:** `ON` iken dosyada kalan artık sayısı C ile aynı (1.955; 1.932 canlı) — C bölünmeden sonra sayfa boşluğunda eski bir kopya bırakıyor, motor artık aynısını yapıyor. **Farksal koşular:** 4.000 tohum, çıktı C ile aynı, her dosya C `integrity_check` geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** üç kipte 99/100, 94/94, 99/100 ("her motor kendi dosyasını kurar" kipi önce 49/100 idi). **Çökme/geri alma:** 0 hata. **Bedel:** toplam 1,006x; komut sayıları `W08` 0,980, `W10` 0,946, `W13` 0,986, `W14` 0,992 (azaldı), `W01` 1,001, `W06` 1,006.
- **Toplam**: `249.74 ms` (taban `248.18 ms`, C `294.77 ms`) — geomean `Rust/C` **`0.822x`** (disk WAL `0.793x`, bellek-içi `0.937x`)
- **Tepe RSS**: `18,184 KiB` (C: `6,372 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.57`** | `7.53` | `7.49` | **`1.010x`** | `1.001x` | `9.65` | **`0.78x`** | `9,276` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.54`** | `7.43` | `7.56` | **`0.998x`** | `1.013x` | `8.65` | **`0.87x`** | `10,116` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.27`** | `27.73` | `28.20` | **`1.003x`** | `1.007x` | `23.64` | **`1.20x`** | `10,116` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.77`** | `7.70` | `7.83` | **`0.992x`** | `0.995x` | `7.50` | **`1.04x`** | `10,180` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `4.28` | **`0.999x`** | `0.999x` | `16.59` | **`0.26x`** | `10,244` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.62`** | `10.61` | `10.95` | **`0.970x`** | `0.983x` | `28.29` | **`0.38x`** | `10,308` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.72`** | `38.58` | `38.63` | **`1.002x`** | `0.999x` | `50.16` | **`0.77x`** | `12,744` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.45`** | `9.38` | `9.79` | **`0.965x`** | `0.988x` | `8.05` | **`1.17x`** | `12,744` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.90`** | `11.84` | `11.84` | **`1.005x`** | `1.003x` | `18.99` | **`0.63x`** | `12,744` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.13`** | `11.08` | `11.20` | **`0.994x`** | `0.990x` | `10.95` | **`1.02x`** | `12,928` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.91`** | `45.71` | `45.79` | **`1.024x`** | `1.028x` | `57.28` | **`0.82x`** | `18,072` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.08`** | `23.40` | `24.65` | **`0.977x`** | `1.014x` | `24.36` | **`0.99x`** | `18,120` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`5.01`** | `4.69` | `4.95` | **`1.012x`** | `1.013x` | `4.13` | **`1.21x`** | `18,152` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.51`** | `36.12` | `36.43` | **`1.002x`** | `1.001x` | `26.55` | **`1.38x`** | `18,120` | `fc7490e2b80c68e2` ✓ |

---

## 04_h3_toplu_kurma — `performans-A-hizalama@fd7364d`

**H3 — `CREATE INDEX` / `REINDEX`, toplu kopyalama, `VACUUM` ve `ALTER TABLE` C'nin yürüttüğü gibi.** (1) **İndeks kurma**: şema satırı önce yazılıyor; anahtarlar kararlı bir sıralayıcıdan geçip C'nin toplu imleciyle (`BTREE_BULKLOAD` → `balance_nonroot(bBulk=1)`) sıralı ekleniyor; `UNIQUE` denetimi bir önceki anahtarla karşılaştırarak yapılıyor; `REINDEX` 3.54.1'in tek döngüsü. Motorun sayfaları doğrudan yazan kendi toplu yükleyicisi kaldırıldı (yapraklar %90 yerine C gibi ~%98–99 dolu). (2) **Aktarım optimizasyonu** (`INSERT INTO d SELECT * FROM s`): C'nin 31 koşulu aynı sırayla; satırlar yeniden kodlanmadan hücre hücre kopyalanıyor, indeksler toplu imleçle; satır kimliği kuralları C ile aynı. (3) **`VACUUM` / `VACUUM INTO`**: C'nin ifade sırası (tablolar, indeksler, tablo başına aktarım, kalan şema satırları, başlık değerleri, dosya kopyası). (4) **`ALTER TABLE`**: şema satırları C'nin iç içe `UPDATE` geçişleriyle yazılıyor; yeniden adlandırma `sqlite_stat1`e dokunmuyor. Ayrıca bir B-tree kuralı: az önce değiştirilen satırın bir sonraki anahtarına C imleci ilerleterek ulaşıyor ve aynı boyutlu kaydın üzerine yazıyor. İki entegrasyon testi beklentisi C'ye göre değişti (indeksli tabloda `VACUUM` örtük satır kimliklerini korur; yeniden adlandırmadan sonra `sqlite_stat1` eski adı tutar).

- **Testler**: `cargo test --workspace`: 810 geçti (18 yeni; 13'ü C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **195 → 215**; kötüleşen yok, 21 iyileşti (17 indeks/`VACUUM` ve 4 yeniden adlandırma senaryosunun hepsi L0). Kalan 30: R-Tree 20, FTS5 7, üç tekil (WITHOUT ROWID silme sırası, taşma sayfalarında önbellek bağımlı baytlar, özel kilit kipinde sayaç, `PRAGMA optimize`, rastgele satır kimliği). **Karar eşliği** (boş dosyadan; 322 betik, 6.514.115 C olayı): 321 betikte olay dizisi aynı, 320'si her yönüyle eşit ve bayt bayt aynı; `bBulk=1` dengeleme adımı C'de 13.016, motorda 13.016; aktarım koşullarının #1–#27 ve #29–#31'i eşit sayıda tetiklendi. Ayrıntı: `karar_esligi_h3.md`. **Sağlık:** `s05` (sonradan indeks) artık bayt bayt aynı, 458/458 sayfa (önce 490/458). **Farksal koşular:** yeni karışık koşu 5.000 tohum — stdout C ile aynı 4.997, her dosya iki yönde bütünlük kontrolünden geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** 99/100, 94/94, 99/100 (değişmedi). **Çökme/geri alma:** dört takım, 0 hata (yeni takım: indeks kurma, `VACUUM`, kopyalama, yeniden adlandırma; 372 betik + 3.794 kurtarma). **Bedel / kazanç:** toplam 0,993x; `W02` 7,45 → 5,64 ms (0,757x; C 8,76 ms). **Geri kazanılmayan:** 84 MB veritabanında beş toplu ifade C'nin 1,3–2,1 katı sürüyor (`VACUUM` 565 ms'ye karşı 393; kopyalama 438'e karşı 210) ve `CREATE INDEX`/`REINDEX` orada H2'den %8/%13 yavaş; H2'ye göre `VACUUM` 2,2 kat, kopyalama 6,1 kat hızlandı. **Hizalanmayan:** `sqlite_rename_quotefix` (yeniden adlandırmada çift tırnaklı dizgelerin çevrilmesi); tablo `UNIQUE` kısıtı ile `CREATE UNIQUE INDEX` ayrımı aktarım uyumluluğunda yok.
- **Toplam**: `248.52 ms` (taban `248.18 ms`, C `294.77 ms`) — geomean `Rust/C` **`0.809x`** (disk WAL `0.778x`, bellek-içi `0.934x`)
- **Tepe RSS**: `18,628 KiB` (C: `6,372 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.89`** | `7.66` | `7.57` | **`1.043x`** | `1.044x` | `9.65` | **`0.82x`** | `9,172` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.69`** | `5.62` | `7.54` | **`0.754x`** | `0.764x` | `8.65` | **`0.66x`** | `10,132` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.42`** | `28.14` | `28.27` | **`1.005x`** | `1.012x` | `23.64` | **`1.20x`** | `10,132` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.87`** | `7.68` | `7.77` | **`1.013x`** | `1.008x` | `7.50` | **`1.05x`** | `10,196` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.07`** | `4.05` | `4.28` | **`0.952x`** | `0.951x` | `16.59` | **`0.24x`** | `10,196` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.79`** | `10.74` | `10.62` | **`1.016x`** | `0.999x` | `28.29` | **`0.38x`** | `10,324` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`37.95`** | `37.70` | `38.72` | **`0.980x`** | `0.979x` | `50.16` | **`0.76x`** | `13,832` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.84`** | `9.61` | `9.45` | **`1.042x`** | `1.029x` | `8.05` | **`1.22x`** | `13,832` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.24`** | `12.05` | `11.90` | **`1.028x`** | `1.032x` | `18.99` | **`0.64x`** | `13,832` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.04`** | `10.87` | `11.13` | **`0.992x`** | `0.982x` | `10.95` | **`1.01x`** | `13,864` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.65`** | `45.45` | `46.91` | **`0.994x`** | `1.022x` | `57.28` | **`0.81x`** | `18,516` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.19`** | `23.78` | `24.08` | **`1.005x`** | `1.019x` | `24.36` | **`0.99x`** | `18,576` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.99`** | `4.74` | `5.01` | **`0.995x`** | `1.008x` | `4.13` | **`1.21x`** | `18,608` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.89`** | `36.71` | `36.51` | **`1.010x`** | `1.012x` | `26.55` | **`1.39x`** | `18,576` | `fc7490e2b80c68e2` ✓ |

---

## 05_h4_rtree — `performans-A-hizalama@b734244`

**H4 — R-Tree, C'nin `rtree.c` dosyasının portu olarak.** `crates/sqliters-ext/src/rtree.rs` yeniden yazıldı: ağaç algoritması (`ChooseLeaf`, `AdjustTree`, `SplitNode`/`splitNodeStartree`/`SortByDimension`, `float`/`double` hücre aritmetiği, silmede `removeNode` + `reinsertNodeContent`), düğüm nesneleri ve **yazma zamanı** (düğüm son başvurusu bırakılınca yazılır; ifadeler arası önbellek ve geri alma kaydı yok), sorgular (`rtreeBestIndex`, arama noktası kuyruğu — satır sırasını belirler, `SQLITE_LOCKED_VTAB`), `rtreecheck`. Motorun kendi ağaç algoritması, sınırsız düğüm önbelleği ve geri alma kaydı kaldırıldı. **Ana motor tarafında tüm sanal tablolar için C'nin kuralları**: değerler `xUpdate`e yakınlık uygulanmadan ulaşır; çok satırlı `DELETE` satır kümesiyle (artan satır kimliği); modülün iç ifadeleri üst düzey ifade gibi sayılır (`changes()`/`total_changes()`); `IN` ve `OR` planları C'nin maliyet modeliyle; sanal tablo taraması için EQP metni; sanal tablo yeniden adlandırma C'nin sırasıyla (ad tırnaklı, görünüm/tetikleyiciler yeniden yazılır). İki pager kuralı: serbest listeden alınan sayfa açık kayıt noktaları için saklanmaz; otomatik commit'te açılan `SAVEPOINT` pager kayıt noktası sayılmaz. Bir entegrasyon testi beklentisi C'ye göre değişti (yeniden adlandırılan sanal tablonun şema metninde ad tırnak içinde).

- **Testler**: `cargo test --workspace`: 832 geçti (25 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **215 → 233**; kötüleşen yok; R-Tree adlı 22 senaryonun hepsi L0. Kalan 12: FTS5 7, beş tekil. **Karar eşliği** (boş dosyadan; 130 R-Tree betiği, 715.577 karar olayı): 130/130 her yönüyle eşit ve bayt bayt aynı; tetiklenmeyen: geometri puanlı arama kuyruğu (geri çağırma yok), bozuk düğüm dalları. Ayrıntı: `karar_esligi_h4.md`. **Farksal koşular:** CLI 3.000 tohum — dosyalar bayt bayt aynı 2.418/2.418 (H3 düzeyinde 0), iki yönde `integrity_check` + `rtreecheck` tamam; API 1.000 tohum — çıktı ve dosyalar 1.000/1.000; karışık yazıcılar (C→Rust, Rust→C) 253 geçmişte son dosya C→C ile aynı. **Sayaçlar** her ifadeden sonra C ile aynı (ör. oluşturma 1/1, kimlikli ekleme toplam +3, güncelleme +5, silme +3). **Bağımsız kontrol:** 7.191 satırlık çok R-Tree'li rastgele betikte dosya, stdout ve stderr C ile aynı. **Sağlık:** s12 73/73 sayfa, 152/152 düğüm; s13 81/81; ikisi de bayt bayt aynı. **Bellek (tepe RSS):** tek işlemde 1e6 ekleme 267 MB → 61,6 MB (C 6,9 MB); modülün kendi belleği sınırlı, kalan pager'ın işlem sayfalarını bellekte tutmasından. 1e5 otomatik commit ekleme: 162,7 G → 8,87 G komut (C 5,06 G). **Çökme/geri alma:** beş takım, 0 hata. **Bedel:** `W12` 23,72 → 33,54 ms (1,414x; komut 1,284x; C 24,63 ms); `W11` 0,936x (ortak sanal tablo altyapısı ucuzladı); toplam 1,029x. Bedelin nedeni C'nin yazma zamanı: her eklemede yol düğümleri okunuyor ve gölge tablolara birkaç B-tree işlemi yapılıyor; C aynı işi 154,6 M komutla yapıyor, motor 190,4 M. **Hizalanmayan:** birleşimde sanal tablo varken birleşim sırası (C'nin yol çözücüsü; `ORDER BY`sız satır sırası ve EQP farklı), geometri/`MATCH` geri çağırmaları, geopoly. FTS5 sayaçları ve EQP `idxNum`/`idxStr` değerleri hâlâ C'ninki değil (FTS5 modülü bu adımda değişmedi).
- **Toplam**: `254.91 ms` (taban `248.18 ms`, C `294.77 ms`) — geomean `Rust/C` **`0.827x`** (disk WAL `0.781x`, bellek-içi `1.019x`)
- **Tepe RSS**: `18,704 KiB` (C: `6,372 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`8.12`** | `7.61` | `7.89` | **`1.029x`** | `1.074x` | `9.65` | **`0.84x`** | `9,136` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.71`** | `5.62` | `5.69` | **`1.004x`** | `0.767x` | `8.65` | **`0.66x`** | `10,092` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.15`** | `27.76` | `28.42` | **`0.990x`** | `1.003x` | `23.64` | **`1.19x`** | `10,092` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.84`** | `7.71` | `7.87` | **`0.996x`** | `1.004x` | `7.50` | **`1.05x`** | `10,156` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.11`** | `4.09` | `4.07` | **`1.009x`** | `0.959x` | `16.59` | **`0.25x`** | `10,156` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.79`** | `10.74` | `10.79` | **`1.001x`** | `1.000x` | `28.29` | **`0.38x`** | `10,288` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`37.71`** | `37.63` | `37.95` | **`0.993x`** | `0.973x` | `50.16` | **`0.75x`** | `13,796` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.94`** | `9.68` | `9.84` | **`1.010x`** | `1.039x` | `8.05` | **`1.24x`** | `13,860` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.11`** | `12.08` | `12.24` | **`0.990x`** | `1.021x` | `18.99` | **`0.64x`** | `13,860` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.28`** | `11.17` | `11.04` | **`1.022x`** | `1.004x` | `10.95` | **`1.03x`** | `13,828` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`42.76`** | `42.12` | `46.65` | **`0.917x`** | `0.937x` | `57.28` | **`0.75x`** | `18,552` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`33.59`** | `31.18` | `24.19` | **`1.389x`** | `1.415x` | `24.36` | **`1.38x`** | `18,576` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.94`** | `4.70` | `4.99` | **`0.991x`** | `0.999x` | `4.13` | **`1.20x`** | `18,608` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`37.86`** | `36.68` | `36.89` | **`1.026x`** | `1.038x` | `26.55` | **`1.43x`** | `18,576` | `fc7490e2b80c68e2` ✓ |

---

## 06_h5_fts5 — `performans-A-hizalama@ba3b169`

**H5 — FTS5 yazma yolu, C'nin `fts5_hash.c` / `fts5_index.c` / `fts5_storage.c` dosyalarının portu olarak.** Yeni `crates/sqliters-ext/src/fts5_index.rs` (5.017 satır): bellek içi karma tablo ve boşaltma eşikleri (`hashsize`, aynı satıra ikinci yazma, küçülen satır kimliği), segment yazıcı (yaprak bölme, `%_idx` ayırıcı anahtarları, doclist indeksi), segment kimliği ve düzey yerleşimi, otomatik birleştirme / kriz birleştirmesi / `fts5StructurePromote`, `optimize` ve `merge`, güvenli silme, mezar taşları ve `contentless_delete`. `fts5.rs` içindeki depolama katmanı C'nin ifade sırasıyla yazıyor (iç ifadeler sayılıyor); `xSync` / `xSavepoint` / `xRelease` / `xRollbackTo` kancaları `fts5_main.c` gibi. Motorun kendi yazıcısı (bekleyen işlem listesi, sıkıştırma ile boşaltma, segment kodlayıcı; `fts5.rs`den 2.336 satır) kaldırıldı. Okuma tarafı segmentleri C sırasıyla ve boşaltılmamış karma tabloyu en yeni kaynak olarak okuyor. `xBestIndex` (`fts5` ve `fts5vocab`) C'nin portu: EQP'deki `idxNum`/`idxStr` (`0:M2`, `0:=`, …) ve maliyetler C ile aynı. **Ana motor tarafında:** sanal tablo kayıt noktası kancaları pager ifade kayıt noktasından önce ve C'nin numaralandırmasıyla; `RELEASE` sırası C gibi; ifade başında `OP_VBegin` (0 satırlık DML ve tetikleyicinin yazdığı sanal tablolar dahil); commit anındaki `xSync`ten sonra `changes()` korunuyor; `SQLITE_CorruptRdOnly` (işlem içinde `SQLITE_CORRUPT` alan ifadeden sonra yazmalar reddedilir, `COMMIT` geri alır); modülün bulduğu bozulma `SQLITE_CORRUPT_VTAB` (267). **Pager:** alt günlük kuralları C'ye getirildi (`subjRequiresPage`, `bTruncateOnRelease`, ana günlüğe yeni giren sayfa alt günlüğe yazılmaz, geri almadan sonra iç kayıtlar durur). Test beklentisi değişmedi.

- **Testler**: `cargo test --workspace`: 853 geçti (21 yeni: 13'ü C'nin yazdığı dosyayla eşitlik, 8'i satır/sayaç), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **233 → 240**; kötüleşen yok; FTS5 senaryolarının 7'si de L0. Kalan 5 (hiçbiri FTS5 değil): WITHOUT ROWID silme sırası (s10), taşma sayfalarında önbellek bağımlı baytlar (s11), özel kilit kipinde değişiklik sayacı, `PRAGMA optimize`, 2^63−1 sonrası rastgele satır kimliği. **Karar eşliği** (boş dosyadan; 131 FTS5 betiği, her iki tarafta 4,10 M karar olayı): 128 betik her yönüyle eşit, 131/131 dosya bayt bayt aynı. Eşit olmayan üç iz: `xRename` içindeki iç `ALTER` ifadelerinin kayıt noktası kancaları (yalnız izde), ve `PRAGMA integrity_check` altında C'nin fazladan bir R-Tree taraması (2 betik, FTS5 dışı). Ayrıntı: `karar_esligi_h5.md`. **Farksal koşular:** CLI 3.000 tohum — saf Rust dosyası C ile bayt bayt aynı 2.939, karışık yazıcılar 2.959; aynı olmayan 55 tohumun 54'ü pager önbellek taşması sınıfı (C `cache_size=1000000` ile koşunca hepsi bayt bayt aynı), kalanlar C'nin kendi bozduğu indeksler (34 tohum; 27'sinde Rust dosyası C'ninkiyle aynı). API 1.000 tohum (hazır ifadeler, 14 tablo yapılandırması): dosyalar 996/1000 bayt bayt aynı, çıktılar 943/1000. **Bağımsız kontrol:** beş yapılandırmada (varsayılan, porter, trigram, `detail=column`, `detail=none`) Rust'ın yazdığı dosya C ile bayt bayt aynı (H4: hiçbiri); sorgular dört yönde aynı; "C dosyası + Rust devam + optimize" = "C devam + optimize"; sayaçlar ve EQP C ile aynı. **Önceki kapılar:** H3 izleri 322/322 (önce 321), H1 95/95, R-Tree 130/130; karışık koşuda ana dosya bayt bayt aynı 1.730/2.000 (önce 1.531; FTS5 194/194, önce 0/194); DML farksalı 377/400 (değişmedi); kurtarma 94/94 + 99/100 (değişmedi). **Çökme/geri alma:** FTS5 takımı dahil 0 hata. **CLI yükleri** (C / H4 / H5, saniye): 4.000 otomatik commit ekleme 0,72 / 9,23 / 1,06 (dosya = C); tek işlemde 1e5 belge 1,19 / 5,24 / 2,63 (tepe bellek 9 / 1.185 / 59 MiB); 1e5 otomatik commit 17,9 / >300 / 28,8; 20.000 belgede 2.000 `UPDATE` 0,30 / 39,7 / 0,63. **Bedel / kazanç:** toplam 0,985x; `W11` 42,99 → 40,34 ms (0,938x; C 57,02 ms); `W12` komut sayısı 1,035x (ifade başına sanal tablo işi). **Hizalanmayan:** `fts5_locale()`; sorgu tarafı okuyucu port değil (C'nin kendi bozduğu indekslerde hata noktası farklı); `LIMIT` ötesindeki satırlarda `snippet()`/`bm25()` hatası (motor sanal tablo taramasını önce topluyor); pager önbellek taşması baytları (büyük işlemler). **Bulunan ayrı sorun:** WITHOUT ROWID + `page_size=65536` içeren bir betik ~2 dakika sürüyor (C 0,016 s; H4'te de var).
- **Toplam**: `249.97 ms` (taban `248.18 ms`, C `294.77 ms`) — geomean `Rust/C` **`0.812x`** (disk WAL `0.770x`, bellek-içi `0.987x`)
- **Tepe RSS**: `17,880 KiB` (C: `6,372 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.66`** | `7.58` | `8.12` | **`0.944x`** | `1.014x` | `9.65` | **`0.79x`** | `9,236` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.70`** | `5.64` | `5.71` | **`0.998x`** | `0.766x` | `8.65` | **`0.66x`** | `10,196` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.88`** | `27.72` | `28.15` | **`0.990x`** | `0.993x` | `23.64` | **`1.18x`** | `10,196` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.69`** | `7.64` | `7.84` | **`0.980x`** | `0.984x` | `7.50` | **`1.03x`** | `10,260` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.10`** | `4.09` | `4.11` | **`0.999x`** | `0.958x` | `16.59` | **`0.25x`** | `10,260` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.62`** | `10.60` | `10.79` | **`0.984x`** | `0.984x` | `28.29` | **`0.38x`** | `10,452` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.30`** | `37.66` | `37.71` | **`1.016x`** | `0.988x` | `50.16` | **`0.76x`** | `13,960` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.74`** | `9.64` | `9.94` | **`0.980x`** | `1.019x` | `8.05` | **`1.21x`** | `13,960` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.17`** | `12.06` | `12.11` | **`1.004x`** | `1.026x` | `18.99` | **`0.64x`** | `13,960` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.00`** | `10.95` | `11.28` | **`0.975x`** | `0.978x` | `10.95` | **`1.00x`** | `13,928` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`40.20`** | `39.72` | `42.76` | **`0.940x`** | `0.881x` | `57.28` | **`0.70x`** | `17,740` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`33.18`** | `32.42` | `33.59` | **`0.988x`** | `1.397x` | `24.36` | **`1.36x`** | `17,760` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.80`** | `4.60` | `4.94` | **`0.972x`** | `0.971x` | `4.13` | **`1.16x`** | `17,792` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.93`** | `36.16` | `37.86` | **`0.975x`** | `1.013x` | `26.55` | **`1.39x`** | `17,760` | `fc7490e2b80c68e2` ✓ |
