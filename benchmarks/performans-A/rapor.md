# `performans-A` — Saf Rust Optimizasyon Adımları Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-12T23:44:22+03:00`
- **Yöntem**: `speedbench` `W01`–`W14`; 1 ısınma + 9 sıralı (interleaved) deneme, medyan; süreçler `taskset -c 20` ile tek çekirdeğe sabit; veritabanı dosyaları `tmpfs (/run/user/1000/sqliters-perf)` üzerinde (fsync gürültüsü yok). Tüm aşamaların saklanmış ikilileri ve C referansı **aynı oturumda** ölçüldü.
- **Kısıtlar (tüm aşamalar)**: `#![forbid(unsafe_code)]`, sıfır dış bağımlılık, SWAR / SIMD / `asm!` yok, SQLite Format-3 ve WAL protokolü uyumu, C ile birebir `FNV-1a` çıktı özeti.

> **Dikkat — farklı donanım.** Üst dizindeki `00`–`03` anlık görüntüleri ve `benchmark_report.md` başka bir makinede (`AMD Ryzen 7 8845HS`) ölçülmüştür; bu dizindeki sayılar o tabloyla doğrudan karşılaştırılamaz. Buradaki `00_performans_taban` satırı, aynı `performans@4ccdc64` kodunun **bu makinedeki** ölçümüdür ve `performans-A` adımlarının bazıdır.
>
> **Ölçülmeyen.** `./sqltest` uçtan uca CLI komut seti (126 örnek) ve C `rss_probe` tabanlı çocuk süreç RSS ölçümü bu aşamalarda koşulmadı; JSON'larda `sqltest_cli_command_set` alanı `null`.

**Dosyalar**: her aşama için `NN_<aşama>.json` (deneme bazında süreler, RSS, özetler); `asamalar.json` (aşama tanımları, commit'ler, test sonuçları); `olc.py` (bu raporu üreten betik); `ek_resmi_run_speedbench_taban.json` (bazın `tools/run_speedbench.py` ile sabitlenmemiş, btrfs üzerindeki ilk resmi koşusu: Rust `723,80 ms` / C `647,26 ms`, geomean `1,12x` — fsync ve zamanlama gürültüsü içerir).

## 1. Aşama Özeti

| Aşama | Git (`dal@commit`) | İkili (`KiB`) | `.text` (`KiB`) | Toplam (`ms`) | Geomean (`vs taban`) | Geomean (`vs C`) | Toplam (`vs C`) | Tepe RSS (`KiB`) | Özetler |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`294.69`** | `—` | **`1.000x`** | **`1.000x`** | `6,220` | `—` |
| **00_performans_taban** | `performans@4ccdc64` | `6,382.7` | `5,412.9` | **`327.06`** | `1.000x` | **`1.180x`** | `1.110x` | `17,924` | 14/14 ✓ |
| **01_a1_wal_islem** | `performans-A@693ac8b` | `6,388.4` | `5,414.5` | **`313.63`** | `0.940x` | **`1.109x`** | `1.064x` | `18,056` | 14/14 ✓ |
| **02_a2_jsonb_indeks** | `performans-A@e2f7843` | `6,417.4` | `5,438.1` | **`287.24`** | `0.858x` | **`1.013x`** | `0.975x` | `17,624` | 14/14 ✓ |
| **03_a3_nokta_dml** | `performans-A@946f61e` | `6,456.6` | `5,472.7` | **`280.89`** | `0.821x` | **`0.969x`** | `0.953x` | `17,776` | 14/14 ✓ |
| **04_a4_kapsayan_indeks** | `performans-A@40e275d` | `6,644.6` | `5,647.8` | **`263.31`** | `0.717x` | **`0.846x`** | `0.894x` | `18,268` | 14/14 ✓ |
| **05_a5_rtree** | `performans-A@700ded4` | `6,650.1` | `5,652.7` | **`242.69`** | `0.684x` | **`0.807x`** | `0.824x` | `17,912` | 14/14 ✓ |
| **05b_duzeltme_dengesiz_agac** | `performans-A@1c12e74` | `6,717.4` | `5,713.6` | **`244.28`** | `0.685x` | **`0.808x`** | `0.829x` | `17,956` | 14/14 ✓ |
| **05d_duzeltme_wal_cokme** | `performans-A@3707ece` | `6,717.5` | `5,713.7` | **`244.84`** | `0.687x` | **`0.811x`** | `0.831x` | `17,880` | 14/14 ✓ |
| **05e_duzeltme_guvence** | `performans-A@e53008f` | `6,734.0` | `5,727.9` | **`245.06`** | `0.688x` | **`0.811x`** | `0.832x` | `17,844` | 14/14 ✓ |
| **05f_duzeltme_fts5** | `performans-A@e4cdaff` | `6,751.0` | `5,734.4` | **`247.00`** | `0.691x` | **`0.816x`** | `0.838x` | `18,216` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_taban | 01_a1_wal_islem | 02_a2_jsonb_indeks | 03_a3_nokta_dml | 04_a4_kapsayan_indeks | 05_a5_rtree | 05b_duzeltme_dengesiz_agac | 05d_duzeltme_wal_cokme | 05e_duzeltme_guvence | 05f_duzeltme_fts5 |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.69` | `7.72` (**0.80x**) | `7.42` (**0.77x**) | `7.44` (**0.77x**) | `7.47` (**0.77x**) | `7.35` (**0.76x**) | `7.44` (**0.77x**) | `7.49` (**0.77x**) | `7.52` (**0.78x**) | `7.44` (**0.77x**) | `7.48` (**0.77x**) |
| `W02_create_indexes` | `8.69` | `9.31` (**1.07x**) | `9.05` (**1.04x**) | `8.46` (**0.97x**) | `7.51` (**0.86x**) | `7.47` (**0.86x**) | `7.64` (**0.88x**) | `7.43` (**0.85x**) | `7.51` (**0.86x**) | `7.54` (**0.87x**) | `7.61` (**0.88x**) |
| `W03_pk_point_select` | `23.44` | `36.57` (**1.56x**) | `27.85` (**1.19x**) | `27.66` (**1.18x**) | `27.70` (**1.18x**) | `27.51` (**1.17x**) | `27.67` (**1.18x**) | `27.83` (**1.19x**) | `27.51` (**1.17x**) | `27.55` (**1.18x**) | `27.56` (**1.18x**) |
| `W04_secondary_idx_range` | `7.46` | `14.36` (**1.92x**) | `12.56` (**1.68x**) | `12.58` (**1.69x**) | `12.64` (**1.69x**) | `7.70` (**1.03x**) | `7.88` (**1.06x**) | `7.82` (**1.05x**) | `7.82` (**1.05x**) | `7.68` (**1.03x**) | `7.75` (**1.04x**) |
| `W05_full_scan_filter_agg` | `16.55` | `17.21` (**1.04x**) | `17.93` (**1.08x**) | `17.61` (**1.06x**) | `18.07` (**1.09x**) | `4.39` (**0.27x**) | `4.31` (**0.26x**) | `4.09` (**0.25x**) | `4.10` (**0.25x**) | `4.31` (**0.26x**) | `4.39` (**0.27x**) |
| `W06_join_group_order` | `28.27` | `10.88` (**0.39x**) | `10.73` (**0.38x**) | `10.56` (**0.37x**) | `10.67` (**0.38x**) | `10.62` (**0.38x**) | `10.41` (**0.37x**) | `10.63` (**0.38x**) | `10.81` (**0.38x**) | `10.61` (**0.38x**) | `10.42` (**0.37x**) |
| `W07_window_functions` | `50.27` | `38.91` (**0.77x**) | `38.85` (**0.77x**) | `38.44` (**0.77x**) | `38.76` (**0.77x**) | `38.45` (**0.77x**) | `38.49` (**0.77x**) | `38.31` (**0.76x**) | `38.40` (**0.76x**) | `38.87` (**0.77x**) | `38.70` (**0.77x**) |
| `W08_indexed_update_delete` | `8.03` | `17.56` (**2.19x**) | `16.38` (**2.04x**) | `15.75` (**1.96x**) | `9.48` (**1.18x**) | `9.51` (**1.18x**) | `9.41` (**1.17x**) | `9.39` (**1.17x**) | `9.41` (**1.17x**) | `9.41` (**1.17x**) | `9.41` (**1.17x**) |
| `W09_like_text_scan` | `18.88` | `11.96` (**0.63x**) | `11.93` (**0.63x**) | `11.88` (**0.63x**) | `11.96` (**0.63x**) | `11.98` (**0.64x**) | `11.82` (**0.63x**) | `11.97` (**0.63x**) | `12.09` (**0.64x**) | `11.84` (**0.63x**) | `11.77` (**0.62x**) |
| `W10_jsonb_extract_agg` | `11.04` | `30.77` (**2.79x**) | `30.79` (**2.79x**) | `11.63` (**1.05x**) | `11.20` (**1.01x**) | `10.98` (**0.99x**) | `10.96` (**0.99x**) | `11.15` (**1.01x**) | `11.10` (**1.00x**) | `11.10` (**1.00x**) | `11.08` (**1.00x**) |
| `W11_fts5_index_and_bm25` | `57.39` | `47.25` (**0.82x**) | `47.99` (**0.84x**) | `46.46` (**0.81x**) | `46.97` (**0.82x**) | `48.67` (**0.85x**) | `45.70` (**0.80x**) | `44.98` (**0.78x**) | `45.26` (**0.79x**) | `44.94` (**0.78x**) | `46.36` (**0.81x**) |
| `W12_rtree_insert_and_bbox` | `24.40` | `40.17` (**1.65x**) | `40.73` (**1.67x**) | `40.04` (**1.64x**) | `39.97` (**1.64x**) | `40.45` (**1.66x**) | `22.92` (**0.94x**) | `23.54` (**0.96x**) | `23.51` (**0.96x**) | `23.69` (**0.97x**) | `24.11` (**0.99x**) |
| `W13_autocommit_wal_txns` | `4.06` | `7.05` (**1.74x**) | `4.95` (**1.22x**) | `4.87` (**1.20x**) | `4.81` (**1.18x**) | `4.88` (**1.20x**) | `4.80` (**1.18x**) | `4.73` (**1.17x**) | `4.81` (**1.18x**) | `4.79` (**1.18x**) | `4.82` (**1.19x**) |
| `W14_cli_sql_script` | `26.51` | `37.33` (**1.41x**) | `36.46` (**1.38x**) | `33.85` (**1.28x**) | `33.68` (**1.27x**) | `33.35` (**1.26x**) | `33.23` (**1.25x**) | `34.91` (**1.32x**) | `35.00` (**1.32x**) | `35.29` (**1.33x**) | `35.56` (**1.34x**) |
| **Toplam** | **`294.69`** | **`327.06`** (**1.11x**) | **`313.63`** (**1.06x**) | **`287.24`** (**0.97x**) | **`280.89`** (**0.95x**) | **`263.31`** (**0.89x**) | **`242.69`** (**0.82x**) | **`244.28`** (**0.83x**) | **`244.84`** (**0.83x**) | **`245.06`** (**0.83x**) | **`247.00`** (**0.84x**) |

---

## 00_performans_taban — `performans@4ccdc64`

**Baz.** `performans` dalının (Saf Rust tavanı, P1-C) bu makinedeki ölçümü; `performans-A` adımlarının karşılaştırma noktası.

- **Testler**: `cargo test --workspace`: 517 geçti, 4 başarısız, 337 yok sayıldı. Başarısız 4 test (`tests/phase7_vtab_framework_and_pragma_tables.rs` içindeki `*_differential_parity`) referans ikili için başka bir makinenin yolunu sabit kodluyor; bu makinede tüm aşamalarda aynı şekilde düşüyor. (C uyumluluk testleri için `sqlite3` 3.54.1 referans ikilisi `PATH`e eklenerek koşuldu.)
- **Toplam**: `327.06 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`1.180x`** (disk WAL `1.094x`, bellek-içi `1.557x`)
- **Tepe RSS**: `17,924 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.72`** | `7.66` | `1.000x` | `9.69` | **`0.80x`** | `8,640` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.31`** | `9.28` | `1.000x` | `8.69` | **`1.07x`** | `9,548` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`36.57`** | `36.37` | `1.000x` | `23.44` | **`1.56x`** | `9,548` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`14.36`** | `14.26` | `1.000x` | `7.46` | **`1.92x`** | `9,612` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.21`** | `17.17` | `1.000x` | `16.55` | **`1.04x`** | `9,612` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.88`** | `10.83` | `1.000x` | `28.27` | **`0.39x`** | `9,612` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.91`** | `38.58` | `1.000x` | `50.27` | **`0.77x`** | `12,124` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`17.56`** | `17.40` | `1.000x` | `8.03` | **`2.19x`** | `12,124` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.96`** | `11.91` | `1.000x` | `18.88` | **`0.63x`** | `12,124` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.77`** | `30.52` | `1.000x` | `11.04` | **`2.79x`** | `13,096` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.25`** | `46.08` | `1.000x` | `57.39` | **`0.82x`** | `17,612` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.17`** | `39.76` | `1.000x` | `24.40` | **`1.65x`** | `17,676` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`7.05`** | `6.79` | `1.000x` | `4.06` | **`1.74x`** | `17,708` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`37.33`** | `36.86` | `1.000x` | `26.51` | **`1.41x`** | `17,680` | `fc7490e2b80c68e2` ✓ |

---

## 01_a1_wal_islem — `performans-A@693ac8b`

**Adım 1 — WAL okuma/yazma işlemi açılış-kapanış maliyeti.** Sistem çağrısı sayısı zaten C ile aynıydı; fark kullanıcı alanındaydı (okuma işlemi başına ~32 mutex turu, tek tek doğrulanan ~70 shm kelime okuması). `validate_offset` satır-içi tek koşul oldu (ayrıntılı hata yolu soğuk fonksiyonda aynen duruyor); bağlantı başına bölge-0 önbelleği, kilitsiz kapalı/zehirli bayrağı, birleşik kilit-sonrası doğrulama (aynı kontroller, aynı sıra); bayt sırasına göre kelime döngülü WAL sağlama toplamı; ardışık WAL çerçeveleri tek konumlu yazmada (≤ 64 KiB) toplanıyor. Hedef: `W03`, `W13`, `W04`.

- **Testler**: `cargo test --workspace`: 517 geçti, bilinen 4 başarısız, 337 yok sayıldı. Windows arka ucu derlenmedi (koda dokunulmadı; bölge-0 önbelleğinin dayandığı değişmez orada yalnızca kod okunarak kontrol edildi).
- **Toplam**: `313.63 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`1.109x`** (disk WAL `1.008x`, bellek-içi `1.573x`)
- **Tepe RSS**: `18,056 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.42`** | `7.34` | `7.72` | **`0.961x`** | `0.961x` | `9.69` | **`0.77x`** | `8,516` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.05`** | `8.98` | `9.31` | **`0.972x`** | `0.972x` | `8.69` | **`1.04x`** | `9,348` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.85`** | `27.65` | `36.57` | **`0.761x`** | `0.761x` | `23.44` | **`1.19x`** | `9,348` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.56`** | `12.47` | `14.36` | **`0.875x`** | `0.875x` | `7.46` | **`1.68x`** | `9,412` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.93`** | `17.76` | `17.21` | **`1.042x`** | `1.042x` | `16.55` | **`1.08x`** | `9,412` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.73`** | `10.70` | `10.88` | **`0.987x`** | `0.987x` | `28.27` | **`0.38x`** | `9,476` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.85`** | `38.55` | `38.91` | **`0.998x`** | `0.998x` | `50.27` | **`0.77x`** | `11,900` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`16.38`** | `16.20` | `17.56` | **`0.933x`** | `0.933x` | `8.03` | **`2.04x`** | `11,900` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.93`** | `11.72` | `11.96` | **`0.997x`** | `0.997x` | `18.88` | **`0.63x`** | `11,900` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.79`** | `30.66` | `30.77` | **`1.001x`** | `1.001x` | `11.04` | **`2.79x`** | `12,936` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.99`** | `47.18` | `47.25` | **`1.016x`** | `1.016x` | `57.39` | **`0.84x`** | `17,692` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.73`** | `39.46` | `40.17` | **`1.014x`** | `1.014x` | `24.40` | **`1.67x`** | `17,732` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.95`** | `4.67` | `7.05` | **`0.702x`** | `0.702x` | `4.06` | **`1.22x`** | `17,764` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.46`** | `36.19` | `37.33` | **`0.977x`** | `0.977x` | `26.51` | **`1.38x`** | `17,732` | `fc7490e2b80c68e2` ✓ |

---

## 02_a2_jsonb_indeks — `performans-A@e2f7843`

**Adım 2 — ham indeks yaprağı yeniden dengeleme ve indeks sıralı akışlı kümeleme.** `W10`daki fark JSON'dan değil iki genel yoldan geliyordu: (1) dolu indeks yaprağına her eklemede üst düğüm + 3 kardeş yaprak hücre hücre `Vec`e ayrıştırılıp yeniden yazılıyordu; `try_insert_index_balance_raw` aynı dağıtımı ham sayfa baytları üzerinde yapıyor (sayfa çıktısı eski yolla bayt bayt aynı, desteklemediği her durumda sayfaya dokunmadan eski yola düşüyor). (2) Akışlı kümeleme planları kapsayıcı olmayan indeks taramasını reddediyordu; yeni `IndexOrderedTableLookup` erişim yolu eklendi. Ayrıca kayıt karşılaştırmada tamsayı-tamsayı hızlı yolu. **Davranış düzeltmesi:** `HashGroupByPlan` tamsayı `1` ile gerçel `1.0` anahtarlarını iki ayrı grup sayıyordu; artık SQLite'taki gibi tek grup. Hedef: `W10`; yan kazanç `W02`, `W08`, `W14`.

- **Testler**: `cargo test --workspace`: 520 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. 9.000 satırlık rastgele indeksli yükte yazılan veritabanı baz ikilininkiyle bayt bayt aynı ve C `integrity_check` geçiyor. Sayfa başına ayrılmış bayt (reserved bytes) kullanan veritabanlarında denenmedi.
- **Toplam**: `287.24 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`1.013x`** (disk WAL `0.986x`, bellek-içi `1.119x`)
- **Tepe RSS**: `17,624 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.44`** | `7.37` | `7.42` | **`1.002x`** | `0.963x` | `9.69` | **`0.77x`** | `8,704` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`8.46`** | `8.40` | `9.05` | **`0.935x`** | `0.909x` | `8.69` | **`0.97x`** | `9,600` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.66`** | `27.54` | `27.85` | **`0.993x`** | `0.756x` | `23.44` | **`1.18x`** | `9,600` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.58`** | `12.46` | `12.56` | **`1.001x`** | `0.876x` | `7.46` | **`1.69x`** | `9,664` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.61`** | `17.50` | `17.93` | **`0.982x`** | `1.023x` | `16.55` | **`1.06x`** | `9,664` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.56`** | `10.50` | `10.73` | **`0.984x`** | `0.971x` | `28.27` | **`0.37x`** | `9,792` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.44`** | `38.06` | `38.85` | **`0.989x`** | `0.988x` | `50.27` | **`0.77x`** | `13,196` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`15.75`** | `15.57` | `16.38` | **`0.962x`** | `0.897x` | `8.03` | **`1.96x`** | `13,260` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.88`** | `11.83` | `11.93` | **`0.996x`** | `0.993x` | `18.88` | **`0.63x`** | `13,260` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.63`** | `11.53` | `30.79` | **`0.378x`** | `0.378x` | `11.04` | **`1.05x`** | `13,292` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.46`** | `45.10` | `47.99` | **`0.968x`** | `0.983x` | `57.39` | **`0.81x`** | `17,492` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.04`** | `39.66` | `40.73` | **`0.983x`** | `0.997x` | `24.40` | **`1.64x`** | `17,496` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.87`** | `4.67` | `4.95` | **`0.984x`** | `0.691x` | `4.06` | **`1.20x`** | `17,528` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.85`** | `33.63` | `36.46` | **`0.928x`** | `0.907x` | `26.51` | **`1.28x`** | `17,496` | `fc7490e2b80c68e2` ✓ |

---

## 03_a3_nokta_dml — `performans-A@946f61e`

**Adım 3 — satır kimliğiyle `UPDATE`/`DELETE` için özel planlar.** Hazırlanmış `UPDATE … WHERE rowid = ?` ve `DELETE … WHERE rowid = ?` genel VDBE yorumlayıcısından geçiyor, satır başına ikinci bir B-tree inişi ve tam kayıt çöz/yeniden kodla yapıyordu. `RowidPointUpdatePlan` / `RowidPointDeletePlan` tek ham arama yapıp değişmeyen alanları ham bayt olarak kopyalıyor; aynı indeks sil/ekle ve satır değiştir çağrılarını aynı baytlarla yapıyor. Yalnızca düz `main` tabloları için (tetikleyici, yabancı anahtar, `CHECK`, `STRICT`, üretilmiş sütun, kısmi/ifade indeksi, özel harmanlama, çakışma yan tümcesi, `RETURNING` yok); diğer her durumda VDBE programı çalışıyor. Ek olarak konumlu tablo işlemleri, ham yaprak yeniden paketleme ve bir kez çözülen indeks arama anahtarı (`RawRecordKey`). Hedef: `W08`; yan kazanç `W02`.

- **Testler**: `cargo test --workspace`: 524 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Rastgele farksal koşu (1.900 tohum, ~2,1 milyon ifade): çıktı, hata kodları, değişiklik sayaçları ve veritabanı baytları adım 2 ile aynı; tüm veritabanları C `integrity_check` geçiyor. Plan ortasında gelen G/Ç hatası / meşgul / kesme durumları hata enjeksiyonuyla denenmedi.
- **Toplam**: `280.89 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`0.969x`** (disk WAL `0.935x`, bellek-içi `1.108x`)
- **Tepe RSS**: `17,776 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.47`** | `7.44` | `7.44` | **`1.005x`** | `0.968x` | `9.69` | **`0.77x`** | `8,668` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.43` | `8.46` | **`0.887x`** | `0.806x` | `8.69` | **`0.86x`** | `9,572` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.70`** | `27.59` | `27.66` | **`1.001x`** | `0.757x` | `23.44` | **`1.18x`** | `9,572` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.64`** | `12.52` | `12.58` | **`1.005x`** | `0.880x` | `7.46` | **`1.69x`** | `9,636` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`18.07`** | `18.01` | `17.61` | **`1.026x`** | `1.050x` | `16.55` | **`1.09x`** | `9,636` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.67`** | `10.58` | `10.56` | **`1.011x`** | `0.981x` | `28.27` | **`0.38x`** | `9,764` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.76`** | `38.30` | `38.44` | **`1.008x`** | `0.996x` | `50.27` | **`0.77x`** | `11,880` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.48`** | `9.38` | `15.75` | **`0.602x`** | `0.540x` | `8.03` | **`1.18x`** | `11,944` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.96`** | `11.67` | `11.88` | **`1.006x`** | `0.999x` | `18.88` | **`0.63x`** | `11,944` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.20`** | `11.15` | `11.63` | **`0.963x`** | `0.364x` | `11.04` | **`1.01x`** | `12,128` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.97`** | `46.31` | `46.46` | **`1.011x`** | `0.994x` | `57.39` | **`0.82x`** | `17,584` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`39.97`** | `39.43` | `40.04` | **`0.998x`** | `0.995x` | `24.40` | **`1.64x`** | `17,676` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.81`** | `4.47` | `4.87` | **`0.988x`** | `0.682x` | `4.06` | **`1.18x`** | `17,708` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.68`** | `33.59` | `33.85` | **`0.995x`** | `0.902x` | `26.51` | **`1.27x`** | `17,676` | `fc7490e2b80c68e2` ✓ |

---

## 04_a4_kapsayan_indeks — `performans-A@40e275d`

**Adım 4 — kapsayan indeks taramalarında tamsayı çekirdeği.** `W04` ve `W05` zamanlarını hücre başına genel işte harcıyordu: ziyaret edilen her hücre için tam kayıt-aralık karşılaştırması, satır başına genel başlık ayrıştırma ve enum dağıtımlı `WHERE`/kümeleme değerlendirmesi. `IntRangeProbe` aralık hedefini arama başına bir kez çözüyor (tüm işlenenler `INTEGER` ve `DESC` sütun yoksa); `RawIndexLeafView` sayfa başına değişmezleri döngü dışına alıyor; `CovIntKernelShape` (`fast_read/int_kernel.rs`), `WHERE` sütun karşılaştırmaları/`BETWEEN` ve kümeler `count/sum/total/avg/min/max` olduğunda satırları tamsayı aralık testi + mevcut `FastSumAccum` ile işliyor (taşma anlambilimi aynı). Çekirdeğin çözemediği her satır ve tamsayı olmayan parametreli her yürütme mevcut genel koddan geçiyor. Hedef: `W04`, `W05`.

- **Testler**: `cargo test --workspace`: 527 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. Farksal koşular: 2.000 tohum (400 bin sorgu, yarısı bozuk indeks sayfalarıyla) ve 700 rastgele aralık/kümeleme sorgusu adım 3 ile birebir aynı; ikincisi ondalık biçimi normalleştirilince C ile de aynı. `W11` (+%3) ve `W09` (+%2,5) aynı komut sayısıyla yavaş okunuyor (kod yerleşimi; bu iş yüklerinin koduna dokunulmadı).
- **Toplam**: `263.31 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`0.846x`** (disk WAL `0.784x`, bellek-içi `1.118x`)
- **Tepe RSS**: `18,268 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.35`** | `7.27` | `7.47` | **`0.984x`** | `0.952x` | `9.69` | **`0.76x`** | `8,804` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.47`** | `7.38` | `7.51` | **`0.995x`** | `0.803x` | `8.69` | **`0.86x`** | `9,640` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.51`** | `27.41` | `27.70` | **`0.993x`** | `0.752x` | `23.44` | **`1.17x`** | `9,640` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.70`** | `7.63` | `12.64` | **`0.609x`** | `0.536x` | `7.46` | **`1.03x`** | `9,704` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.39`** | `4.37` | `18.07` | **`0.243x`** | `0.255x` | `16.55` | **`0.27x`** | `9,704` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.62`** | `10.58` | `10.67` | **`0.995x`** | `0.976x` | `28.27` | **`0.38x`** | `9,900` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.45`** | `38.02` | `38.76` | **`0.992x`** | `0.988x` | `50.27` | **`0.77x`** | `13,356` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.51`** | `9.43` | `9.48` | **`1.003x`** | `0.542x` | `8.03` | **`1.18x`** | `13,356` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.98`** | `11.73` | `11.96` | **`1.002x`** | `1.002x` | `18.88` | **`0.64x`** | `13,356` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.98`** | `10.91` | `11.20` | **`0.981x`** | `0.357x` | `11.04` | **`0.99x`** | `13,388` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`48.67`** | `46.88` | `46.97` | **`1.036x`** | `1.030x` | `57.39` | **`0.85x`** | `17,976` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.45`** | `39.54` | `39.97` | **`1.012x`** | `1.007x` | `24.40` | **`1.66x`** | `18,028` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.88`** | `4.59` | `4.81` | **`1.015x`** | `0.692x` | `4.06` | **`1.20x`** | `18,060` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.35`** | `32.97` | `33.68` | **`0.990x`** | `0.893x` | `26.51` | **`1.26x`** | `18,028` | `fc7490e2b80c68e2` ✓ |

---

## 05_a5_rtree — `performans-A@700ded4`

**Adım 5 — R-Tree yerinde düğüm güncelleme ve sanal tablo işlem sırasının belirlenimci yapılması.** `W12`nin süresi disk G/Ç'si değil CPU işiydi: bölünmeyen eklemelerde ve üst düğüm sınır kutusu düzeltmesinde düğüm `klonla → değiştir → geri koy` yerine yerinde değiştiriliyor, tam düğüm anlık görüntüsü yerine küçük geri alma kayıtları tutuluyor; düğüm sınır kutusu önbelleği, tek sorgulu önbellek erişimi, düğüm numaralı haritalar için ucuz hasher; `SELECT`in `WHERE` ifadesi yürütme başına 4 kez derin kopyalanıyordu, artık ödünç alınıyor. Gölge tablo baytları değişmedi. **Belirlenimsizlik düzeltmesi (tabanda da vardı, doğrulama sırasında bulundu):** aynı işlemde iki veya daha çok sanal tablo değişince dosya, süreç başına rastgele seçilen birkaç bayt-farklı varyanttan biri oluyordu (işlem yayılımı rastgele tohumlu `HashMap` sırasıyla yürüyordu). Artık tablolar işleme katılma sırasıyla işleniyor (C'deki `db->aVTrans` sırası). Mantıksal içerik hiç etkilenmiyordu; fark yalnızca gölge tablolar arasındaki sayfa tahsis sırasındaydı. Hedef: `W12`; yan kazanç `W11`.

- **Testler**: `cargo test --workspace`: 533 geçti (6 yeni), bilinen 4 başarısız, 337 yok sayıldı. Çok sanal tablolu iki tekrar betiği 16'şar koşuda tek dosya üretiyor (önceden iki); çıktı adım 4 ile aynı, C `rtreecheck` + `integrity_check` `ok`. Farksal koşu: 650 tek R-Tree tohumu çıktı ve dosya baytlarında adım 4 ile aynı; 300 çok R-Tree tohumu çıktıda aynı ve koşudan koşuya kendisiyle aynı. Dokunulmayan bilinen hatalar (tabanda var): 512 bayt sayfalı bazı R-Tree dosyalarında C `integrity_check` → `Child page depth differs`; R-Tree satır kimliği tahsisi `i64::MAX`ta taşıyor. Ayrıntı: `saglik/c_uyumluluk_ve_saglik_analizi.md`.
- **Toplam**: `242.69 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`0.807x`** (disk WAL `0.782x`, bellek-içi `0.905x`)
- **Tepe RSS**: `17,912 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.44`** | `7.42` | `7.35` | **`1.013x`** | `0.964x` | `9.69` | **`0.77x`** | `8,952` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.64`** | `7.59` | `7.47` | **`1.022x`** | `0.821x` | `8.69` | **`0.88x`** | `9,792` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.67`** | `27.45` | `27.51` | **`1.006x`** | `0.757x` | `23.44` | **`1.18x`** | `9,792` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.88`** | `7.74` | `7.70` | **`1.024x`** | `0.549x` | `7.46` | **`1.06x`** | `9,856` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.31`** | `4.30` | `4.39` | **`0.982x`** | `0.250x` | `16.55` | **`0.26x`** | `9,856` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.41`** | `10.34` | `10.62` | **`0.980x`** | `0.957x` | `28.27` | **`0.37x`** | `10,048` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.49`** | `38.10` | `38.45` | **`1.001x`** | `0.989x` | `50.27` | **`0.77x`** | `12,528` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.41`** | `9.37` | `9.51` | **`0.989x`** | `0.536x` | `8.03` | **`1.17x`** | `12,528` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.82`** | `11.75` | `11.98` | **`0.987x`** | `0.988x` | `18.88` | **`0.63x`** | `12,528` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.96`** | `10.90` | `10.98` | **`0.999x`** | `0.356x` | `11.04` | **`0.99x`** | `12,776` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.70`** | `45.11` | `48.67` | **`0.939x`** | `0.967x` | `57.39` | **`0.80x`** | `17,684` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`22.92`** | `22.64` | `40.45` | **`0.567x`** | `0.571x` | `24.40` | **`0.94x`** | `17,732` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.80`** | `4.49` | `4.88` | **`0.983x`** | `0.681x` | `4.06` | **`1.18x`** | `17,764` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.23`** | `33.06` | `33.35` | **`0.996x`** | `0.890x` | `26.51` | **`1.25x`** | `17,732` | `fc7490e2b80c68e2` ✓ |

---

## 05b_duzeltme_dengesiz_agac — `performans-A@1c12e74`

**Adım 5b — doğruluk düzeltmesi (performans amacı yok).** Üç hata da ilk port commit'inden (`814e41e`) beri vardı. (1) **Dengesiz tablo B-tree'si:** silme, kök olmayan bir iç sayfayı tek çocukla bırakınca o sayfa serbest bırakılıp üst düğüm doğrudan toruna bağlanıyordu; alt ağaç kardeşlerinden bir seviye kısa kalıyordu. C bunu bozulma sayıyor (`Child page depth differs`), motorun kendisi de sonradan aynı ağaçta hata verip satır kaybedebiliyordu. Artık sayfa korunup aynı seviyedeki kardeşleriyle C'deki gibi yeniden dengeleniyor; derinlik yalnızca kökte değişiyor. (2) **İndeks sayfasının hücre sayısına göre bölünmesi:** çok farklı boyutlu anahtarlarda bir yarı sayfaya sığmıyor ve `INSERT` `cells exceed usable B-tree page capacity` ile düşüyordu (C'de başarılı); bölünme noktası artık iki yarının da sığdığı en yakın konum (önceden çalışan bölünmeler bayt bayt aynı). (3) **`PRAGMA integrity_check`:** C'nin `btree.c` kontrolleri C'nin mesaj metinleriyle port edildi — B-tree düzeyinde 30 kontrolün 29'u (önce 6), SQL düzeyinde 24'ün 18'i (önce 2) C ile aynı. Ayrıntı: `saglik/integrity_check_parity.md`.

- **Testler**: `cargo test --workspace`: 542 geçti (9 yeni), bilinen 4 başarısız, 337 yok sayıldı. Üç tekrar betiği artık C `integrity_check` + `rtreecheck` geçiyor ve ağaç başına tek yaprak derinliği var; yeni Rust kontrolü düzeltme öncesi yazılmış bozuk dosyalarda C ile aynı mesajı veriyor. Hataya hiç girmeyen 13 senaryonun dosyaları önceki commit ile bayt bayt aynı. Farksal koşu: 12.960 + 1.920 çalıştırma, her dosya C `integrity_check` geçiyor. **Hız:** `W01`–`W13` komut sayıları değişmedi (hiçbir iş yükü düzeltilen yollara girmiyor; önceki kazançların hiçbiri bu hatalara dayanmıyordu). `W14` +%5: betik `PRAGMA integrity_check` ile bitiyor ve kontrol artık C'nin yaptığı işi yapıyor (eski 2,0 M komut, C 5,4 M, yeni 14,4 M). Doğrulanmayan: commit bazında ikiye bölme; `auto_vacuum` + iç sayfa yeniden dengeleme için ayrı C farksalı; kasıtlı bozulmuş 6.000 dosyanın 5'inde C hata verirken motor vermiyor (sayfa 1 hasarını şema yükleyici kabul ediyor).
- **Toplam**: `244.28 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`0.808x`** (disk WAL `0.781x`, bellek-içi `0.914x`)
- **Tepe RSS**: `17,956 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.49`** | `7.47` | `7.44` | **`1.006x`** | `0.970x` | `9.69` | **`0.77x`** | `8,836` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.43`** | `7.36` | `7.64` | **`0.973x`** | `0.798x` | `8.69` | **`0.85x`** | `9,676` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.83`** | `27.51` | `27.67` | **`1.006x`** | `0.761x` | `23.44` | **`1.19x`** | `9,676` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.82`** | `7.77` | `7.88` | **`0.993x`** | `0.545x` | `7.46` | **`1.05x`** | `9,740` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.09`** | `4.08` | `4.31` | **`0.950x`** | `0.238x` | `16.55` | **`0.25x`** | `9,740` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.61` | `10.41` | **`1.021x`** | `0.977x` | `28.27` | **`0.38x`** | `9,932` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.31`** | `38.15` | `38.49` | **`0.995x`** | `0.984x` | `50.27` | **`0.76x`** | `12,412` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.39`** | `9.35` | `9.41` | **`0.998x`** | `0.535x` | `8.03` | **`1.17x`** | `12,412` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.97`** | `11.94` | `11.82` | **`1.012x`** | `1.001x` | `18.88` | **`0.63x`** | `12,412` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.15`** | `11.03` | `10.96` | **`1.017x`** | `0.362x` | `11.04` | **`1.01x`** | `12,660` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`44.98`** | `44.55` | `45.70` | **`0.984x`** | `0.952x` | `57.39` | **`0.78x`** | `17,652` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.54`** | `23.10` | `22.92` | **`1.027x`** | `0.586x` | `24.40` | **`0.96x`** | `17,688` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.73`** | `4.55` | `4.80` | **`0.986x`** | `0.671x` | `4.06` | **`1.17x`** | `17,720` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`34.91`** | `34.59` | `33.23` | **`1.051x`** | `0.935x` | `26.51` | **`1.32x`** | `17,704` | `fc7490e2b80c68e2` ✓ |

---

## 05d_duzeltme_wal_cokme — `performans-A@3707ece`

**Adım 5d — doğruluk düzeltmesi: WAL çökme atomikliği (performans amacı yok).** 5c envanterinde bulundu; WAL yazıcısı port edildiğinden beri vardı. Büyük bir yazma işleminde sayfa önbelleği taşıp sayfalar commit'ten önce `-wal`e yazıldığında ve taşmış bir sayfa yeniden değiştiğinde, `COMMIT` eski çerçeveyi yerinde yeniden yazar ve sonuna (commit çerçevesi dahil) yeni çerçeveler ekler. C bu sırada eklenen çerçeveleri tuz ve sağlama toplamı yerine 16 sıfır baytla yazar (`walEncodeFrame`, `iReCksum != 0`); geçerli zinciri ancak `walRewriteChecksums` kurar. Motor bu çerçeveleri gerçek tuz ve eski görüntüler üzerinden zincirlenmiş sağlama toplamıyla yazıyordu: çökme, eklenen çerçeveleri koruyup yerinde yeniden yazımı kaybederse **yarım uygulanmış bir işlem** görünür oluyordu (C aynı durumda işlemi atar). Artık C ile aynı: bekleyen yeniden yazım varken eklenen çerçeveler doğrulanamaz biçimde yazılıyor. Yazma sırası ve adım 1'deki çerçeve toplama değişmedi.

- **Testler**: `cargo test --workspace`: 546 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Dört yeni test, senkronlanmamış `-wal` yazımlarının her alt kümesini (test başına ~1.000–4.100 çökme görüntüsü) dener; kurtarma ya işlem öncesi durumu ya da işlemin tamamını vermeli. Düzeltme geri alınınca dördü de düşüyor. Süreç öldürme + blok geri yükleme betiği: düzeltmeden önce 600 satır ve iki güncellemeden biri uygulanmış; sonra C yazıcısıyla aynı sonuç (işlem atılıyor). **Hız:** toplam 0,999x; ölçüm iş yükleri önbellek taşırmadığı için `W13`/`W01` komut ve sistem çağrısı sayıları aynı. Doğrulanmayan: gerçek güç kesintisi (tüm çökme durumları sentetik). Kod okunarak bulunan ve düzeltilmeyen iki ara-durum farkı daha var (`journal_size_limit` kırpma hatası; `load_deserialized_image` içinde senkronsuz kapanış checkpoint'i).
- **Toplam**: `244.84 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`0.811x`** (disk WAL `0.785x`, bellek-içi `0.914x`)
- **Tepe RSS**: `17,880 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.52`** | `7.45` | `7.49` | **`1.004x`** | `0.974x` | `9.69` | **`0.78x`** | `8,868` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.44` | `7.43` | **`1.010x`** | `0.806x` | `8.69` | **`0.86x`** | `9,708` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.51`** | `27.39` | `27.83` | **`0.989x`** | `0.752x` | `23.44` | **`1.17x`** | `9,708` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.82`** | `7.76` | `7.82` | **`0.999x`** | `0.544x` | `7.46` | **`1.05x`** | `9,772` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.10`** | `4.09` | `4.09` | **`1.002x`** | `0.238x` | `16.55` | **`0.25x`** | `9,772` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.81`** | `10.77` | `10.63` | **`1.017x`** | `0.993x` | `28.27` | **`0.38x`** | `9,964` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.40`** | `38.18` | `38.31` | **`1.002x`** | `0.987x` | `50.27` | **`0.76x`** | `12,508` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.41`** | `9.31` | `9.39` | **`1.002x`** | `0.536x` | `8.03` | **`1.17x`** | `12,508` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.09`** | `11.89` | `11.97` | **`1.010x`** | `1.011x` | `18.88` | **`0.64x`** | `12,508` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.10`** | `11.02` | `11.15` | **`0.995x`** | `0.361x` | `11.04` | **`1.00x`** | `12,756` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.26`** | `44.67` | `44.98` | **`1.006x`** | `0.958x` | `57.39` | **`0.79x`** | `17,808` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.51`** | `23.25` | `23.54` | **`0.999x`** | `0.585x` | `24.40` | **`0.96x`** | `17,880` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.81`** | `4.54` | `4.73` | **`1.017x`** | `0.683x` | `4.06` | **`1.18x`** | `17,912` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.00`** | `34.72` | `34.91` | **`1.003x`** | `0.938x` | `26.51` | **`1.32x`** | `17,880` | `fc7490e2b80c68e2` ✓ |

---

## 05e_duzeltme_guvence — `performans-A@e53008f`

**Adım 5e — doğruluk düzeltmeleri: güvence ve uyumluluk hataları (performans amacı yok).** (1) `PRAGMA secure_delete` yok sayılıyordu (satır dönmüyor, silinen hücrelerin içeriği dosyada kalıyordu); tüm biçimleriyle uygulandı, `ON` iken dosyada yalnızca canlı satırların baytları kalıyor. (2) C'de satır dönen ama burada dönmeyen pragmalar: `analysis_limit` (saklanıyor ve `ANALYZE` tarafından uygulanıyor), `data_version`, `cell_size_check` (yüklenen sayfalar C'deki gibi doğrulanıyor), `mmap_size`. (3) `ALTER TABLE ADD COLUMN` sütun metnini AST'den yeniden üretiyordu: `DEFAULT x'00'` → `x''` (varsayılan değer **kayboluyordu**), sabit olmayan varsayılan kabul ediliyordu; artık kullanıcının metni C'deki gibi aynen ekleniyor, C'nin kontrolleri C'nin sırası ve mesajlarıyla çalışıyor. (4) Tablosu olmayan veritabanının başlığı: şema biçimi ve kodlama ilk şema satırına kadar 0 kalıyor ve `PRAGMA encoding` o ana kadar geçerli. (5) İki WAL hata yolu: `journal_size_limit` kırpma hatası artık commit çerçevesi kalıcı olmuş bir `COMMIT`i başarısız yapmıyor; `load_deserialized_image` WAL'i senkronlu checkpoint ile kapatıyor.

- **Testler**: `cargo test --workspace`: 559 geçti (13 yeni), bilinen 4 başarısız, 337 yok sayıldı. `ADD COLUMN` farksalı 1.126 durumda C ile eşit: 1.118 (önce 407). Ayrışma bulucu (192 senaryo): hiçbir senaryo daha kötü katmanda bitmiyor, 11'i iyileşti. 13 düzen senaryosu önceki commit ile bayt bayt aynı. **Hız:** toplam 0,998x; komut sayıları aynı (`W08`/`W09` +%0,05). `W05` +%5 aynı komut sayısıyla (kod yerleşimi). **Bilerek C'den farklı bırakılan (mevcut entegrasyon testleri eski davranışı doğruluyor):** `ADD COLUMN` satır kontrolü hatası `SQLITE_CONSTRAINT` dönüyor (C: `SQLITE_ERROR`); `too many columns on t` (C: `… on sqlite_altertab_t`). Doğrulanmayan: deserialize yolu çalışan bir C ikilisine karşı.
- **Toplam**: `245.06 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`0.811x`** (disk WAL `0.785x`, bellek-içi `0.914x`)
- **Tepe RSS**: `17,844 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.44`** | `7.37` | `7.52` | **`0.989x`** | `0.963x` | `9.69` | **`0.77x`** | `8,736` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.54`** | `7.49` | `7.51` | **`1.004x`** | `0.810x` | `8.69` | **`0.87x`** | `9,576` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.55`** | `27.32` | `27.51` | **`1.001x`** | `0.753x` | `23.44` | **`1.18x`** | `9,576` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.68`** | `7.62` | `7.82` | **`0.983x`** | `0.535x` | `7.46` | **`1.03x`** | `9,640` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.31`** | `4.29` | `4.10` | **`1.050x`** | `0.250x` | `16.55` | **`0.26x`** | `9,640` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.61`** | `10.58` | `10.81` | **`0.982x`** | `0.975x` | `28.27` | **`0.38x`** | `9,832` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.87`** | `38.53` | `38.40` | **`1.012x`** | `0.999x` | `50.27` | **`0.77x`** | `12,440` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.41`** | `9.38` | `9.41` | **`1.001x`** | `0.536x` | `8.03` | **`1.17x`** | `12,440` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.84`** | `11.79` | `12.09` | **`0.979x`** | `0.990x` | `18.88` | **`0.63x`** | `12,440` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.10`** | `11.07` | `11.10` | **`1.000x`** | `0.361x` | `11.04` | **`1.00x`** | `12,688` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`44.94`** | `44.56` | `45.26` | **`0.993x`** | `0.951x` | `57.39` | **`0.78x`** | `17,588` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.69`** | `23.17` | `23.51` | **`1.007x`** | `0.590x` | `24.40` | **`0.97x`** | `17,632` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.79`** | `4.43` | `4.81` | **`0.995x`** | `0.679x` | `4.06` | **`1.18x`** | `17,664` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.29`** | `34.75` | `35.00` | **`1.008x`** | `0.945x` | `26.51` | **`1.33x`** | `17,652` | `fc7490e2b80c68e2` ✓ |

---

## 05f_duzeltme_fts5 — `performans-A@e4cdaff`

**Adım 5f — doğruluk düzeltmesi: FTS5'in C ile iki yönlü uyumu (performans amacı yok).** Bu değişiklikten önce iki motor bir FTS5 tablosunu güvenle paylaşamıyordu (farksal koşuda 300 tohumun 300'ü en az bir yönde başarısızdı). Düzeltilenler: **silme bayrağı** (okuma: C'nin `UPDATE`inden sonra `MATCH` satır kaçırıyor, ardından `'optimize'` o kayıtları kalıcı siliyordu; yazma: aynı boşaltmada silinip yeniden eklenen satırın bayrağı kayboluyordu); **64 KiB'ı aşan yaprak** (C: bozuk); **`contentless_delete=1`** kayıt biçimi; **ayraçlar** (unicode61, ascii, porter, trigram C'den bayt düzeyinde port edildi — katlama/aksan/kök bulma farklıydı, C `checksum mismatch` veriyordu); **çakışma yan tümceleri** (`INSERT OR REPLACE` vb.); **`contentless_unindexed=1`, `locale=1`**; FTS5 içinde gerçel→metin çevrimi. Ayrıca sorgu dilbilgisi, `bm25`/`rank`, `highlight`, `snippet`, `fts5vocab`, özel komutlar ve hata metinleri C'ye getirildi. **Kapsam dışı (yapısal, ayrı karar):** segment birleştirme takvimi ve boşaltma zamanı, yaprak boyutu hedefi ve doclist indeksi, yapı çerezi.

- **Testler**: `cargo test --workspace`: 597 geçti (38 yeni; 34'ü C CLI'sinden üretilmiş, gömülü C gölge tablo görüntüleriyle), bilinen 4 başarısız, 337 yok sayıldı. Yeni 34 durumun 32'si önceki kaynakta düşüyor. Farksal koşu (rastgele yapılandırma, işlemler ve sorgular; Rust yazar / C yazar / sırayla yazar; dört motor-dosya birleşiminde bütünlük kontrolü): 3.600 tohum / 10.800 çalıştırma, 0 hata (önce 900 çalıştırmanın 863'ü). 60 tohum yalnızca `secure-delete=1` ile farklı; her biri C'nin kendi indeksini bozmasına dayanıyor (51'i yalnız C'nin yazdığı geçmişte). **Hız:** toplam 1,004x; `W11` +%2,6 (komut sayısı 248,20 M → 247,86 M; `bm25`in C ile birebir aynı olması için eşleşen satır başına daha çok bellek). Düzeltilmeyen: `fts5_locale()` yok; `LIMIT` ötesindeki satırlar için yardımcı fonksiyon değerlendirmesi; `%_config` sürümü bağlanırken denetlenmiyor.
- **Toplam**: `247.00 ms` (taban `327.06 ms`, C `294.69 ms`) — geomean `Rust/C` **`0.816x`** (disk WAL `0.787x`, bellek-içi `0.929x`)
- **Tepe RSS**: `18,216 KiB` (C: `6,220 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.48`** | `7.40` | `7.44` | **`1.005x`** | `0.968x` | `9.69` | **`0.77x`** | `9,024` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.61`** | `7.54` | `7.54` | **`1.009x`** | `0.817x` | `8.69` | **`0.88x`** | `9,864` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.56`** | `27.39` | `27.55` | **`1.000x`** | `0.753x` | `23.44` | **`1.18x`** | `9,864` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.75`** | `7.65` | `7.68` | **`1.008x`** | `0.540x` | `7.46` | **`1.04x`** | `9,992` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.39`** | `4.38` | `4.31` | **`1.020x`** | `0.255x` | `16.55` | **`0.27x`** | `9,992` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.42`** | `10.40` | `10.61` | **`0.982x`** | `0.958x` | `28.27` | **`0.37x`** | `10,056` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.70`** | `38.36` | `38.87` | **`0.996x`** | `0.994x` | `50.27` | **`0.77x`** | `12,536` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.41`** | `9.36` | `9.41` | **`0.999x`** | `0.536x` | `8.03` | **`1.17x`** | `12,536` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.77`** | `11.68` | `11.84` | **`0.994x`** | `0.984x` | `18.88` | **`0.62x`** | `12,536` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.08`** | `11.08` | `11.10` | **`0.999x`** | `0.360x` | `11.04` | **`1.00x`** | `12,720` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.36`** | `45.38` | `44.94` | **`1.032x`** | `0.981x` | `57.39` | **`0.81x`** | `18,056` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.11`** | `23.28` | `23.69` | **`1.018x`** | `0.600x` | `24.40` | **`0.99x`** | `18,104` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.82`** | `4.52` | `4.79` | **`1.006x`** | `0.683x` | `4.06` | **`1.19x`** | `18,136` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.56`** | `35.00` | `35.29` | **`1.008x`** | `0.952x` | `26.51` | **`1.34x`** | `14,972` | `fc7490e2b80c68e2` ✓ |
