# `performans-A` — Saf Rust Optimizasyon Adımları Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-13T05:58:27+03:00`
- **Yöntem**: `speedbench` `W01`–`W14`; 1 ısınma + 9 sıralı (interleaved) deneme, medyan; süreçler `taskset -c 20` ile tek çekirdeğe sabit; veritabanı dosyaları `tmpfs (/run/user/1000/sqliters-perf)` üzerinde (fsync gürültüsü yok). Tüm aşamaların saklanmış ikilileri ve C referansı **aynı oturumda** ölçüldü.
- **Kısıtlar (tüm aşamalar)**: `#![forbid(unsafe_code)]`, sıfır dış bağımlılık, SWAR / SIMD / `asm!` yok, SQLite Format-3 ve WAL protokolü uyumu, C ile birebir `FNV-1a` çıktı özeti.

> **Dikkat — farklı donanım.** Üst dizindeki `00`–`03` anlık görüntüleri ve `benchmark_report.md` başka bir makinede (`AMD Ryzen 7 8845HS`) ölçülmüştür; bu dizindeki sayılar o tabloyla doğrudan karşılaştırılamaz. Buradaki `00_performans_taban` satırı, aynı `performans@4ccdc64` kodunun **bu makinedeki** ölçümüdür ve `performans-A` adımlarının bazıdır.
>
> **Ölçülmeyen.** `./sqltest` uçtan uca CLI komut seti (126 örnek) ve C `rss_probe` tabanlı çocuk süreç RSS ölçümü bu aşamalarda koşulmadı; JSON'larda `sqltest_cli_command_set` alanı `null`.

**Dosyalar**: her aşama için `NN_<aşama>.json` (deneme bazında süreler, RSS, özetler); `asamalar.json` (aşama tanımları, commit'ler, test sonuçları); `olc.py` (bu raporu üreten betik); `ek_resmi_run_speedbench_taban.json` (bazın `tools/run_speedbench.py` ile sabitlenmemiş, btrfs üzerindeki ilk resmi koşusu: Rust `723,80 ms` / C `647,26 ms`, geomean `1,12x` — fsync ve zamanlama gürültüsü içerir).

## 1. Aşama Özeti

| Aşama | Git (`dal@commit`) | İkili (`KiB`) | `.text` (`KiB`) | Toplam (`ms`) | Geomean (`vs taban`) | Geomean (`vs C`) | Toplam (`vs C`) | Tepe RSS (`KiB`) | Özetler |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`294.97`** | `—` | **`1.000x`** | **`1.000x`** | `6,256` | `—` |
| **00_performans_taban** | `performans@4ccdc64` | `6,382.7` | `5,412.9` | **`328.37`** | `1.000x` | **`1.185x`** | `1.113x` | `17,972` | 14/14 ✓ |
| **01_a1_wal_islem** | `performans-A@693ac8b` | `6,388.4` | `5,414.5` | **`314.64`** | `0.938x` | **`1.111x`** | `1.067x` | `17,928` | 14/14 ✓ |
| **02_a2_jsonb_indeks** | `performans-A@e2f7843` | `6,417.4` | `5,438.1` | **`289.16`** | `0.857x` | **`1.014x`** | `0.980x` | `17,628` | 14/14 ✓ |
| **03_a3_nokta_dml** | `performans-A@946f61e` | `6,456.6` | `5,472.7` | **`281.32`** | `0.820x` | **`0.971x`** | `0.954x` | `17,652` | 14/14 ✓ |
| **04_a4_kapsayan_indeks** | `performans-A@40e275d` | `6,644.6` | `5,647.8` | **`264.08`** | `0.715x` | **`0.847x`** | `0.895x` | `18,280` | 14/14 ✓ |
| **05_a5_rtree** | `performans-A@700ded4` | `6,650.1` | `5,652.7` | **`244.26`** | `0.685x` | **`0.811x`** | `0.828x` | `17,844` | 14/14 ✓ |
| **05b_duzeltme_dengesiz_agac** | `performans-A@1c12e74` | `6,717.4` | `5,713.6` | **`244.45`** | `0.683x` | **`0.809x`** | `0.829x` | `17,868` | 14/14 ✓ |
| **05d_duzeltme_wal_cokme** | `performans-A@3707ece` | `6,717.5` | `5,713.7` | **`246.47`** | `0.689x` | **`0.815x`** | `0.836x` | `17,964` | 14/14 ✓ |
| **05e_duzeltme_guvence** | `performans-A@e53008f` | `6,734.0` | `5,727.9` | **`245.69`** | `0.687x` | **`0.814x`** | `0.833x` | `17,836` | 14/14 ✓ |
| **05f_duzeltme_fts5** | `performans-A@e4cdaff` | `6,751.0` | `5,734.4` | **`246.97`** | `0.689x` | **`0.817x`** | `0.837x` | `18,240` | 14/14 ✓ |
| **05g_hizalama_dosya** | `performans-A@fc4f0ca` | `6,802.1` | `5,778.2` | **`247.71`** | `0.691x` | **`0.819x`** | `0.840x` | `18,104` | 14/14 ✓ |
| **05h_hizalama_sql** | `performans-A@a0cffa4` | `6,883.8` | `5,850.2` | **`248.53`** | `0.693x` | **`0.821x`** | `0.843x` | `18,156` | 14/14 ✓ |
| **05i_duzeltme_yedekleme** | `performans-A@39deab5` | `6,884.3` | `5,850.6` | **`248.74`** | `0.695x` | **`0.824x`** | `0.843x` | `18,308` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_taban | 01_a1_wal_islem | 02_a2_jsonb_indeks | 03_a3_nokta_dml | 04_a4_kapsayan_indeks | 05_a5_rtree | 05b_duzeltme_dengesiz_agac | 05d_duzeltme_wal_cokme | 05e_duzeltme_guvence | 05f_duzeltme_fts5 | 05g_hizalama_dosya | 05h_hizalama_sql | 05i_duzeltme_yedekleme |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.64` | `7.71` (**0.80x**) | `7.42` (**0.77x**) | `7.43` (**0.77x**) | `7.47` (**0.78x**) | `7.35` (**0.76x**) | `7.47` (**0.78x**) | `7.51` (**0.78x**) | `7.50` (**0.78x**) | `7.43` (**0.77x**) | `7.46` (**0.77x**) | `7.57` (**0.79x**) | `7.62` (**0.79x**) | `7.59` (**0.79x**) |
| `W02_create_indexes` | `8.67` | `9.32` (**1.07x**) | `9.07` (**1.05x**) | `8.46` (**0.98x**) | `7.51` (**0.87x**) | `7.50` (**0.86x**) | `7.59` (**0.88x**) | `7.46` (**0.86x**) | `7.57` (**0.87x**) | `7.58` (**0.88x**) | `7.58` (**0.88x**) | `7.51` (**0.87x**) | `7.65` (**0.88x**) | `7.74` (**0.89x**) |
| `W03_pk_point_select` | `23.70` | `36.93` (**1.56x**) | `27.96` (**1.18x**) | `27.80` (**1.17x**) | `28.04` (**1.18x**) | `28.19` (**1.19x**) | `27.89` (**1.18x**) | `27.78` (**1.17x**) | `28.21` (**1.19x**) | `27.64` (**1.17x**) | `27.79` (**1.17x**) | `28.00` (**1.18x**) | `28.34` (**1.20x**) | `27.91` (**1.18x**) |
| `W04_secondary_idx_range` | `7.51` | `14.42` (**1.92x**) | `12.55` (**1.67x**) | `12.65` (**1.68x**) | `12.58` (**1.68x**) | `7.75` (**1.03x**) | `7.90` (**1.05x**) | `7.84` (**1.04x**) | `7.87` (**1.05x**) | `7.75` (**1.03x**) | `7.79` (**1.04x**) | `7.84` (**1.04x**) | `7.81` (**1.04x**) | `7.93` (**1.06x**) |
| `W05_full_scan_filter_agg` | `16.58` | `17.24` (**1.04x**) | `17.98` (**1.08x**) | `17.58` (**1.06x**) | `18.00` (**1.08x**) | `4.40` (**0.27x**) | `4.32` (**0.26x**) | `4.10` (**0.25x**) | `4.11` (**0.25x**) | `4.31` (**0.26x**) | `4.41` (**0.27x**) | `4.38` (**0.26x**) | `4.08` (**0.25x**) | `4.28` (**0.26x**) |
| `W06_join_group_order` | `28.23` | `10.87` (**0.39x**) | `10.74` (**0.38x**) | `10.55` (**0.37x**) | `10.66` (**0.38x**) | `10.67` (**0.38x**) | `10.36` (**0.37x**) | `10.66` (**0.38x**) | `10.81` (**0.38x**) | `10.62` (**0.38x**) | `10.47` (**0.37x**) | `10.51` (**0.37x**) | `11.11` (**0.39x**) | `10.90` (**0.39x**) |
| `W07_window_functions` | `50.25` | `39.01` (**0.78x**) | `38.85` (**0.77x**) | `38.77` (**0.77x**) | `38.83` (**0.77x**) | `38.44` (**0.77x**) | `38.51` (**0.77x**) | `38.37` (**0.76x**) | `38.61` (**0.77x**) | `38.83` (**0.77x**) | `38.86` (**0.77x**) | `39.08` (**0.78x**) | `38.67` (**0.77x**) | `38.82` (**0.77x**) |
| `W08_indexed_update_delete` | `8.03` | `17.54` (**2.18x**) | `16.40` (**2.04x**) | `15.71` (**1.96x**) | `9.43` (**1.17x**) | `9.56` (**1.19x**) | `9.41` (**1.17x**) | `9.39` (**1.17x**) | `9.36` (**1.17x**) | `9.44` (**1.18x**) | `9.42` (**1.17x**) | `9.45` (**1.18x**) | `9.43` (**1.17x**) | `9.43` (**1.17x**) |
| `W09_like_text_scan` | `18.88` | `11.98` (**0.64x**) | `11.74` (**0.62x**) | `11.90` (**0.63x**) | `11.95` (**0.63x**) | `11.78` (**0.62x**) | `12.03` (**0.64x**) | `11.96` (**0.63x**) | `11.95` (**0.63x**) | `11.83` (**0.63x**) | `11.79` (**0.62x**) | `11.92` (**0.63x**) | `12.03` (**0.64x**) | `12.01` (**0.64x**) |
| `W10_jsonb_extract_agg` | `10.94` | `30.66` (**2.80x**) | `30.76` (**2.81x**) | `11.61` (**1.06x**) | `11.19` (**1.02x**) | `10.93` (**1.00x**) | `10.97` (**1.00x**) | `11.09` (**1.01x**) | `11.23` (**1.03x**) | `11.08` (**1.01x**) | `11.10` (**1.01x**) | `11.17` (**1.02x**) | `11.09` (**1.01x**) | `11.17` (**1.02x**) |
| `W11_fts5_index_and_bm25` | `57.42` | `47.48` (**0.83x**) | `48.93` (**0.85x**) | `47.51` (**0.83x**) | `46.99` (**0.82x**) | `48.90` (**0.85x**) | `45.97` (**0.80x**) | `45.24` (**0.79x**) | `45.25` (**0.79x**) | `45.21` (**0.79x**) | `46.18` (**0.80x**) | `46.41` (**0.81x**) | `45.50` (**0.79x**) | `46.04` (**0.80x**) |
| `W12_rtree_insert_and_bbox` | `24.47` | `40.76` (**1.67x**) | `40.50` (**1.66x**) | `40.51` (**1.66x**) | `40.11` (**1.64x**) | `40.30` (**1.65x**) | `23.51` (**0.96x**) | `23.37` (**0.95x**) | `24.06` (**0.98x**) | `23.65` (**0.97x**) | `23.62` (**0.96x**) | `23.72` (**0.97x**) | `24.24` (**0.99x**) | `24.05` (**0.98x**) |
| `W13_autocommit_wal_txns` | `4.04` | `7.21` (**1.79x**) | `4.99` (**1.24x**) | `4.74` (**1.18x**) | `4.89` (**1.21x**) | `4.87` (**1.21x**) | `4.79` (**1.19x**) | `4.79` (**1.19x**) | `4.84` (**1.20x**) | `4.87` (**1.21x**) | `4.86` (**1.21x**) | `4.87` (**1.21x**) | `4.87` (**1.21x**) | `4.89` (**1.21x**) |
| `W14_cli_sql_script` | `26.62` | `37.25` (**1.40x**) | `36.75` (**1.38x**) | `33.95` (**1.27x**) | `33.66` (**1.26x**) | `33.46` (**1.26x**) | `33.55` (**1.26x**) | `34.89` (**1.31x**) | `35.10` (**1.32x**) | `35.44` (**1.33x**) | `35.64` (**1.34x**) | `35.28` (**1.33x**) | `36.10` (**1.36x**) | `35.98` (**1.35x**) |
| **Toplam** | **`294.97`** | **`328.37`** (**1.11x**) | **`314.64`** (**1.07x**) | **`289.16`** (**0.98x**) | **`281.32`** (**0.95x**) | **`264.08`** (**0.90x**) | **`244.26`** (**0.83x**) | **`244.45`** (**0.83x**) | **`246.47`** (**0.84x**) | **`245.69`** (**0.83x**) | **`246.97`** (**0.84x**) | **`247.71`** (**0.84x**) | **`248.53`** (**0.84x**) | **`248.74`** (**0.84x**) |

---

## 00_performans_taban — `performans@4ccdc64`

**Baz.** `performans` dalının (Saf Rust tavanı, P1-C) bu makinedeki ölçümü; `performans-A` adımlarının karşılaştırma noktası.

- **Testler**: `cargo test --workspace`: 517 geçti, 4 başarısız, 337 yok sayıldı. Başarısız 4 test (`tests/phase7_vtab_framework_and_pragma_tables.rs` içindeki `*_differential_parity`) referans ikili için başka bir makinenin yolunu sabit kodluyor; bu makinede tüm aşamalarda aynı şekilde düşüyor. (C uyumluluk testleri için `sqlite3` 3.54.1 referans ikilisi `PATH`e eklenerek koşuldu.)
- **Toplam**: `328.37 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`1.185x`** (disk WAL `1.097x`, bellek-içi `1.569x`)
- **Tepe RSS**: `17,972 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.71`** | `7.70` | `1.000x` | `9.64` | **`0.80x`** | `8,596` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.32`** | `9.27` | `1.000x` | `8.67` | **`1.07x`** | `9,504` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`36.93`** | `36.46` | `1.000x` | `23.70` | **`1.56x`** | `9,504` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`14.42`** | `14.25` | `1.000x` | `7.51` | **`1.92x`** | `9,568` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.24`** | `17.17` | `1.000x` | `16.58` | **`1.04x`** | `9,568` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.87`** | `10.82` | `1.000x` | `28.23` | **`0.39x`** | `9,572` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`39.01`** | `38.60` | `1.000x` | `50.25` | **`0.78x`** | `12,084` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`17.54`** | `17.36` | `1.000x` | `8.03` | **`2.18x`** | `12,084` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.98`** | `11.92` | `1.000x` | `18.88` | **`0.64x`** | `12,084` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.66`** | `30.50` | `1.000x` | `10.94` | **`2.80x`** | `13,056` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.48`** | `46.36` | `1.000x` | `57.42` | **`0.83x`** | `17,800` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.76`** | `39.68` | `1.000x` | `24.47` | **`1.67x`** | `17,848` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`7.21`** | `7.11` | `1.000x` | `4.04` | **`1.79x`** | `17,880` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`37.25`** | `37.19` | `1.000x` | `26.62` | **`1.40x`** | `15,736` | `fc7490e2b80c68e2` ✓ |

---

## 01_a1_wal_islem — `performans-A@693ac8b`

**Adım 1 — WAL okuma/yazma işlemi açılış-kapanış maliyeti.** Sistem çağrısı sayısı zaten C ile aynıydı; fark kullanıcı alanındaydı (okuma işlemi başına ~32 mutex turu, tek tek doğrulanan ~70 shm kelime okuması). `validate_offset` satır-içi tek koşul oldu (ayrıntılı hata yolu soğuk fonksiyonda aynen duruyor); bağlantı başına bölge-0 önbelleği, kilitsiz kapalı/zehirli bayrağı, birleşik kilit-sonrası doğrulama (aynı kontroller, aynı sıra); bayt sırasına göre kelime döngülü WAL sağlama toplamı; ardışık WAL çerçeveleri tek konumlu yazmada (≤ 64 KiB) toplanıyor. Hedef: `W03`, `W13`, `W04`.

- **Testler**: `cargo test --workspace`: 517 geçti, bilinen 4 başarısız, 337 yok sayıldı. Windows arka ucu derlenmedi (koda dokunulmadı; bölge-0 önbelleğinin dayandığı değişmez orada yalnızca kod okunarak kontrol edildi).
- **Toplam**: `314.64 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`1.111x`** (disk WAL `1.009x`, bellek-içi `1.583x`)
- **Tepe RSS**: `17,928 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.42`** | `7.33` | `7.71` | **`0.962x`** | `0.962x` | `9.64` | **`0.77x`** | `8,648` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.07`** | `8.99` | `9.32` | **`0.974x`** | `0.974x` | `8.67` | **`1.05x`** | `9,480` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.96`** | `27.76` | `36.93` | **`0.757x`** | `0.757x` | `23.70` | **`1.18x`** | `9,480` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.55`** | `12.44` | `14.42` | **`0.870x`** | `0.870x` | `7.51` | **`1.67x`** | `9,608` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.98`** | `17.82` | `17.24` | **`1.043x`** | `1.043x` | `16.58` | **`1.08x`** | `9,608` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.74`** | `10.68` | `10.87` | **`0.988x`** | `0.988x` | `28.23` | **`0.38x`** | `9,672` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.85`** | `38.57` | `39.01` | **`0.996x`** | `0.996x` | `50.25` | **`0.77x`** | `12,032` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`16.40`** | `16.31` | `17.54` | **`0.935x`** | `0.935x` | `8.03` | **`2.04x`** | `12,032` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.74`** | `11.68` | `11.98` | **`0.980x`** | `0.980x` | `18.88` | **`0.62x`** | `12,032` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.76`** | `30.65` | `30.66` | **`1.003x`** | `1.003x` | `10.94` | **`2.81x`** | `13,068` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`48.93`** | `47.51` | `47.48` | **`1.031x`** | `1.031x` | `57.42` | **`0.85x`** | `17,768` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.50`** | `39.74` | `40.76` | **`0.994x`** | `0.994x` | `24.47` | **`1.66x`** | `17,800` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.99`** | `4.70` | `7.21` | **`0.692x`** | `0.692x` | `4.04` | **`1.24x`** | `17,832` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.75`** | `36.45` | `37.25` | **`0.987x`** | `0.987x` | `26.62` | **`1.38x`** | `17,800` | `fc7490e2b80c68e2` ✓ |

---

## 02_a2_jsonb_indeks — `performans-A@e2f7843`

**Adım 2 — ham indeks yaprağı yeniden dengeleme ve indeks sıralı akışlı kümeleme.** `W10`daki fark JSON'dan değil iki genel yoldan geliyordu: (1) dolu indeks yaprağına her eklemede üst düğüm + 3 kardeş yaprak hücre hücre `Vec`e ayrıştırılıp yeniden yazılıyordu; `try_insert_index_balance_raw` aynı dağıtımı ham sayfa baytları üzerinde yapıyor (sayfa çıktısı eski yolla bayt bayt aynı, desteklemediği her durumda sayfaya dokunmadan eski yola düşüyor). (2) Akışlı kümeleme planları kapsayıcı olmayan indeks taramasını reddediyordu; yeni `IndexOrderedTableLookup` erişim yolu eklendi. Ayrıca kayıt karşılaştırmada tamsayı-tamsayı hızlı yolu. **Davranış düzeltmesi:** `HashGroupByPlan` tamsayı `1` ile gerçel `1.0` anahtarlarını iki ayrı grup sayıyordu; artık SQLite'taki gibi tek grup. Hedef: `W10`; yan kazanç `W02`, `W08`, `W14`.

- **Testler**: `cargo test --workspace`: 520 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. 9.000 satırlık rastgele indeksli yükte yazılan veritabanı baz ikilininkiyle bayt bayt aynı ve C `integrity_check` geçiyor. Sayfa başına ayrılmış bayt (reserved bytes) kullanan veritabanlarında denenmedi.
- **Toplam**: `289.16 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`1.014x`** (disk WAL `0.984x`, bellek-içi `1.132x`)
- **Tepe RSS**: `17,628 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.43`** | `7.37` | `7.42` | **`1.002x`** | `0.964x` | `9.64` | **`0.77x`** | `8,604` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`8.46`** | `8.42` | `9.07` | **`0.932x`** | `0.908x` | `8.67` | **`0.98x`** | `9,436` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.80`** | `27.57` | `27.96` | **`0.994x`** | `0.753x` | `23.70` | **`1.17x`** | `9,436` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.65`** | `12.49` | `12.55` | **`1.008x`** | `0.877x` | `7.51` | **`1.68x`** | `9,500` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.58`** | `17.48` | `17.98` | **`0.978x`** | `1.020x` | `16.58` | **`1.06x`** | `9,564` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.55`** | `10.53` | `10.74` | **`0.983x`** | `0.971x` | `28.23` | **`0.37x`** | `9,628` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.77`** | `38.06` | `38.85` | **`0.998x`** | `0.994x` | `50.25` | **`0.77x`** | `13,096` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`15.71`** | `15.55` | `16.40` | **`0.958x`** | `0.896x` | `8.03` | **`1.96x`** | `13,096` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.90`** | `11.84` | `11.74` | **`1.014x`** | `0.994x` | `18.88` | **`0.63x`** | `13,096` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.61`** | `11.50` | `30.76` | **`0.377x`** | `0.379x` | `10.94` | **`1.06x`** | `13,064` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.51`** | `46.63` | `48.93` | **`0.971x`** | `1.001x` | `57.42` | **`0.83x`** | `17,460` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.51`** | `39.92` | `40.50` | **`1.000x`** | `0.994x` | `24.47` | **`1.66x`** | `17,528` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.74`** | `4.39` | `4.99` | **`0.949x`** | `0.657x` | `4.04` | **`1.18x`** | `17,560` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.95`** | `33.47` | `36.75` | **`0.924x`** | `0.911x` | `26.62` | **`1.27x`** | `17,528` | `fc7490e2b80c68e2` ✓ |

---

## 03_a3_nokta_dml — `performans-A@946f61e`

**Adım 3 — satır kimliğiyle `UPDATE`/`DELETE` için özel planlar.** Hazırlanmış `UPDATE … WHERE rowid = ?` ve `DELETE … WHERE rowid = ?` genel VDBE yorumlayıcısından geçiyor, satır başına ikinci bir B-tree inişi ve tam kayıt çöz/yeniden kodla yapıyordu. `RowidPointUpdatePlan` / `RowidPointDeletePlan` tek ham arama yapıp değişmeyen alanları ham bayt olarak kopyalıyor; aynı indeks sil/ekle ve satır değiştir çağrılarını aynı baytlarla yapıyor. Yalnızca düz `main` tabloları için (tetikleyici, yabancı anahtar, `CHECK`, `STRICT`, üretilmiş sütun, kısmi/ifade indeksi, özel harmanlama, çakışma yan tümcesi, `RETURNING` yok); diğer her durumda VDBE programı çalışıyor. Ek olarak konumlu tablo işlemleri, ham yaprak yeniden paketleme ve bir kez çözülen indeks arama anahtarı (`RawRecordKey`). Hedef: `W08`; yan kazanç `W02`.

- **Testler**: `cargo test --workspace`: 524 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Rastgele farksal koşu (1.900 tohum, ~2,1 milyon ifade): çıktı, hata kodları, değişiklik sayaçları ve veritabanı baytları adım 2 ile aynı; tüm veritabanları C `integrity_check` geçiyor. Plan ortasında gelen G/Ç hatası / meşgul / kesme durumları hata enjeksiyonuyla denenmedi.
- **Toplam**: `281.32 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.971x`** (disk WAL `0.936x`, bellek-içi `1.111x`)
- **Tepe RSS**: `17,652 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.47`** | `7.43` | `7.43` | **`1.006x`** | `0.969x` | `9.64` | **`0.78x`** | `8,608` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.41` | `8.46` | **`0.888x`** | `0.806x` | `8.67` | **`0.87x`** | `9,444` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.04`** | `27.89` | `27.80` | **`1.009x`** | `0.759x` | `23.70` | **`1.18x`** | `9,444` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.58`** | `12.50` | `12.65` | **`0.995x`** | `0.873x` | `7.51` | **`1.68x`** | `9,508` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`18.00`** | `17.96` | `17.58` | **`1.023x`** | `1.044x` | `16.58` | **`1.08x`** | `9,508` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.66`** | `10.58` | `10.55` | **`1.010x`** | `0.981x` | `28.23` | **`0.38x`** | `9,576` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.83`** | `38.41` | `38.77` | **`1.002x`** | `0.995x` | `50.25` | **`0.77x`** | `11,692` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.43`** | `9.33` | `15.71` | **`0.600x`** | `0.538x` | `8.03` | **`1.17x`** | `11,692` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.95`** | `11.69` | `11.90` | **`1.004x`** | `0.998x` | `18.88` | **`0.63x`** | `11,692` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.19`** | `11.14` | `11.61` | **`0.964x`** | `0.365x` | `10.94` | **`1.02x`** | `11,940` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.99`** | `46.16` | `47.51` | **`0.989x`** | `0.990x` | `57.42` | **`0.82x`** | `17,208` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.11`** | `39.34` | `40.51` | **`0.990x`** | `0.984x` | `24.47` | **`1.64x`** | `17,248` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.89`** | `4.65` | `4.74` | **`1.032x`** | `0.678x` | `4.04` | **`1.21x`** | `17,280` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.66`** | `33.57` | `33.95` | **`0.992x`** | `0.904x` | `26.62` | **`1.26x`** | `17,248` | `fc7490e2b80c68e2` ✓ |

---

## 04_a4_kapsayan_indeks — `performans-A@40e275d`

**Adım 4 — kapsayan indeks taramalarında tamsayı çekirdeği.** `W04` ve `W05` zamanlarını hücre başına genel işte harcıyordu: ziyaret edilen her hücre için tam kayıt-aralık karşılaştırması, satır başına genel başlık ayrıştırma ve enum dağıtımlı `WHERE`/kümeleme değerlendirmesi. `IntRangeProbe` aralık hedefini arama başına bir kez çözüyor (tüm işlenenler `INTEGER` ve `DESC` sütun yoksa); `RawIndexLeafView` sayfa başına değişmezleri döngü dışına alıyor; `CovIntKernelShape` (`fast_read/int_kernel.rs`), `WHERE` sütun karşılaştırmaları/`BETWEEN` ve kümeler `count/sum/total/avg/min/max` olduğunda satırları tamsayı aralık testi + mevcut `FastSumAccum` ile işliyor (taşma anlambilimi aynı). Çekirdeğin çözemediği her satır ve tamsayı olmayan parametreli her yürütme mevcut genel koddan geçiyor. Hedef: `W04`, `W05`.

- **Testler**: `cargo test --workspace`: 527 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. Farksal koşular: 2.000 tohum (400 bin sorgu, yarısı bozuk indeks sayfalarıyla) ve 700 rastgele aralık/kümeleme sorgusu adım 3 ile birebir aynı; ikincisi ondalık biçimi normalleştirilince C ile de aynı. `W11` (+%3) ve `W09` (+%2,5) aynı komut sayısıyla yavaş okunuyor (kod yerleşimi; bu iş yüklerinin koduna dokunulmadı).
- **Toplam**: `264.08 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.847x`** (disk WAL `0.785x`, bellek-içi `1.119x`)
- **Tepe RSS**: `18,280 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.35`** | `7.26` | `7.47` | **`0.983x`** | `0.953x` | `9.64` | **`0.76x`** | `8,924` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.50`** | `7.41` | `7.51` | **`0.998x`** | `0.805x` | `8.67` | **`0.86x`** | `9,764` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.19`** | `27.52` | `28.04` | **`1.005x`** | `0.763x` | `23.70` | **`1.19x`** | `9,764` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.75`** | `7.68` | `12.58` | **`0.616x`** | `0.538x` | `7.51` | **`1.03x`** | `9,828` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.40`** | `4.37` | `18.00` | **`0.244x`** | `0.255x` | `16.58` | **`0.27x`** | `9,828` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.67`** | `10.58` | `10.66` | **`1.001x`** | `0.982x` | `28.23` | **`0.38x`** | `9,956` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.44`** | `38.12` | `38.83` | **`0.990x`** | `0.985x` | `50.25` | **`0.77x`** | `13,348` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.56`** | `9.44` | `9.43` | **`1.014x`** | `0.545x` | `8.03` | **`1.19x`** | `13,348` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.78`** | `11.75` | `11.95` | **`0.986x`** | `0.984x` | `18.88` | **`0.62x`** | `13,348` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.93`** | `10.91` | `11.19` | **`0.977x`** | `0.357x` | `10.94` | **`1.00x`** | `13,380` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`48.90`** | `45.70` | `46.99` | **`1.041x`** | `1.030x` | `57.42` | **`0.85x`** | `18,164` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.30`** | `39.48` | `40.11` | **`1.005x`** | `0.989x` | `24.47` | **`1.65x`** | `18,280` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.87`** | `4.72` | `4.89` | **`0.994x`** | `0.675x` | `4.04` | **`1.21x`** | `18,312` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.46`** | `33.02` | `33.66` | **`0.994x`** | `0.898x` | `26.62` | **`1.26x`** | `18,280` | `fc7490e2b80c68e2` ✓ |

---

## 05_a5_rtree — `performans-A@700ded4`

**Adım 5 — R-Tree yerinde düğüm güncelleme ve sanal tablo işlem sırasının belirlenimci yapılması.** `W12`nin süresi disk G/Ç'si değil CPU işiydi: bölünmeyen eklemelerde ve üst düğüm sınır kutusu düzeltmesinde düğüm `klonla → değiştir → geri koy` yerine yerinde değiştiriliyor, tam düğüm anlık görüntüsü yerine küçük geri alma kayıtları tutuluyor; düğüm sınır kutusu önbelleği, tek sorgulu önbellek erişimi, düğüm numaralı haritalar için ucuz hasher; `SELECT`in `WHERE` ifadesi yürütme başına 4 kez derin kopyalanıyordu, artık ödünç alınıyor. Gölge tablo baytları değişmedi. **Belirlenimsizlik düzeltmesi (tabanda da vardı, doğrulama sırasında bulundu):** aynı işlemde iki veya daha çok sanal tablo değişince dosya, süreç başına rastgele seçilen birkaç bayt-farklı varyanttan biri oluyordu (işlem yayılımı rastgele tohumlu `HashMap` sırasıyla yürüyordu). Artık tablolar işleme katılma sırasıyla işleniyor (C'deki `db->aVTrans` sırası). Mantıksal içerik hiç etkilenmiyordu; fark yalnızca gölge tablolar arasındaki sayfa tahsis sırasındaydı. Hedef: `W12`; yan kazanç `W11`.

- **Testler**: `cargo test --workspace`: 533 geçti (6 yeni), bilinen 4 başarısız, 337 yok sayıldı. Çok sanal tablolu iki tekrar betiği 16'şar koşuda tek dosya üretiyor (önceden iki); çıktı adım 4 ile aynı, C `rtreecheck` + `integrity_check` `ok`. Farksal koşu: 650 tek R-Tree tohumu çıktı ve dosya baytlarında adım 4 ile aynı; 300 çok R-Tree tohumu çıktıda aynı ve koşudan koşuya kendisiyle aynı. Dokunulmayan bilinen hatalar (tabanda var): 512 bayt sayfalı bazı R-Tree dosyalarında C `integrity_check` → `Child page depth differs`; R-Tree satır kimliği tahsisi `i64::MAX`ta taşıyor. Ayrıntı: `saglik/c_uyumluluk_ve_saglik_analizi.md`.
- **Toplam**: `244.26 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.811x`** (disk WAL `0.784x`, bellek-içi `0.917x`)
- **Tepe RSS**: `17,844 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.47`** | `7.43` | `7.35` | **`1.017x`** | `0.969x` | `9.64` | **`0.78x`** | `8,784` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.59`** | `7.57` | `7.50` | **`1.012x`** | `0.814x` | `8.67` | **`0.88x`** | `9,620` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.89`** | `27.65` | `28.19` | **`0.990x`** | `0.755x` | `23.70` | **`1.18x`** | `9,620` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.90`** | `7.77` | `7.75` | **`1.019x`** | `0.548x` | `7.51` | **`1.05x`** | `9,684` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.32`** | `4.30` | `4.40` | **`0.982x`** | `0.251x` | `16.58` | **`0.26x`** | `9,684` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.36`** | `10.33` | `10.67` | **`0.971x`** | `0.953x` | `28.23` | **`0.37x`** | `9,880` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.51`** | `38.23` | `38.44` | **`1.002x`** | `0.987x` | `50.25` | **`0.77x`** | `12,424` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.41`** | `9.40` | `9.56` | **`0.985x`** | `0.537x` | `8.03` | **`1.17x`** | `12,424` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.03`** | `11.79` | `11.78` | **`1.021x`** | `1.004x` | `18.88` | **`0.64x`** | `12,424` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.97`** | `10.93` | `10.93` | **`1.003x`** | `0.358x` | `10.94` | **`1.00x`** | `12,672` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.97`** | `45.45` | `48.90` | **`0.940x`** | `0.968x` | `57.42` | **`0.80x`** | `17,684` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.51`** | `22.65` | `40.30` | **`0.583x`** | `0.577x` | `24.47` | **`0.96x`** | `17,740` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.79`** | `4.59` | `4.87` | **`0.984x`** | `0.664x` | `4.04` | **`1.19x`** | `17,772` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.55`** | `33.03` | `33.46` | **`1.003x`** | `0.901x` | `26.62` | **`1.26x`** | `17,740` | `fc7490e2b80c68e2` ✓ |

---

## 05b_duzeltme_dengesiz_agac — `performans-A@1c12e74`

**Adım 5b — doğruluk düzeltmesi (performans amacı yok).** Üç hata da ilk port commit'inden (`814e41e`) beri vardı. (1) **Dengesiz tablo B-tree'si:** silme, kök olmayan bir iç sayfayı tek çocukla bırakınca o sayfa serbest bırakılıp üst düğüm doğrudan toruna bağlanıyordu; alt ağaç kardeşlerinden bir seviye kısa kalıyordu. C bunu bozulma sayıyor (`Child page depth differs`), motorun kendisi de sonradan aynı ağaçta hata verip satır kaybedebiliyordu. Artık sayfa korunup aynı seviyedeki kardeşleriyle C'deki gibi yeniden dengeleniyor; derinlik yalnızca kökte değişiyor. (2) **İndeks sayfasının hücre sayısına göre bölünmesi:** çok farklı boyutlu anahtarlarda bir yarı sayfaya sığmıyor ve `INSERT` `cells exceed usable B-tree page capacity` ile düşüyordu (C'de başarılı); bölünme noktası artık iki yarının da sığdığı en yakın konum (önceden çalışan bölünmeler bayt bayt aynı). (3) **`PRAGMA integrity_check`:** C'nin `btree.c` kontrolleri C'nin mesaj metinleriyle port edildi — B-tree düzeyinde 30 kontrolün 29'u (önce 6), SQL düzeyinde 24'ün 18'i (önce 2) C ile aynı. Ayrıntı: `saglik/integrity_check_parity.md`.

- **Testler**: `cargo test --workspace`: 542 geçti (9 yeni), bilinen 4 başarısız, 337 yok sayıldı. Üç tekrar betiği artık C `integrity_check` + `rtreecheck` geçiyor ve ağaç başına tek yaprak derinliği var; yeni Rust kontrolü düzeltme öncesi yazılmış bozuk dosyalarda C ile aynı mesajı veriyor. Hataya hiç girmeyen 13 senaryonun dosyaları önceki commit ile bayt bayt aynı. Farksal koşu: 12.960 + 1.920 çalıştırma, her dosya C `integrity_check` geçiyor. **Hız:** `W01`–`W13` komut sayıları değişmedi (hiçbir iş yükü düzeltilen yollara girmiyor; önceki kazançların hiçbiri bu hatalara dayanmıyordu). `W14` +%5: betik `PRAGMA integrity_check` ile bitiyor ve kontrol artık C'nin yaptığı işi yapıyor (eski 2,0 M komut, C 5,4 M, yeni 14,4 M). Doğrulanmayan: commit bazında ikiye bölme; `auto_vacuum` + iç sayfa yeniden dengeleme için ayrı C farksalı; kasıtlı bozulmuş 6.000 dosyanın 5'inde C hata verirken motor vermiyor (sayfa 1 hasarını şema yükleyici kabul ediyor).
- **Toplam**: `244.45 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.809x`** (disk WAL `0.782x`, bellek-içi `0.914x`)
- **Tepe RSS**: `17,868 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.51`** | `7.46` | `7.47` | **`1.005x`** | `0.974x` | `9.64` | **`0.78x`** | `8,920` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.46`** | `7.40` | `7.59` | **`0.983x`** | `0.800x` | `8.67` | **`0.86x`** | `9,756` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.78`** | `27.59` | `27.89` | **`0.996x`** | `0.752x` | `23.70` | **`1.17x`** | `9,756` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.84`** | `7.77` | `7.90` | **`0.993x`** | `0.544x` | `7.51` | **`1.04x`** | `9,820` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.10`** | `4.08` | `4.32` | **`0.949x`** | `0.238x` | `16.58` | **`0.25x`** | `9,820` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.66`** | `10.62` | `10.36` | **`1.029x`** | `0.981x` | `28.23` | **`0.38x`** | `10,016` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.37`** | `38.12` | `38.51` | **`0.997x`** | `0.984x` | `50.25` | **`0.76x`** | `12,560` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.39`** | `9.36` | `9.41` | **`0.998x`** | `0.536x` | `8.03` | **`1.17x`** | `12,560` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.96`** | `11.93` | `12.03` | **`0.995x`** | `0.999x` | `18.88` | **`0.63x`** | `12,560` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.09`** | `11.03` | `10.97` | **`1.011x`** | `0.362x` | `10.94` | **`1.01x`** | `12,808` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.24`** | `44.11` | `45.97` | **`0.984x`** | `0.953x` | `57.42` | **`0.79x`** | `17,792` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.37`** | `23.07` | `23.51` | **`0.994x`** | `0.573x` | `24.47` | **`0.95x`** | `17,868` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.79`** | `4.49` | `4.79` | **`1.001x`** | `0.664x` | `4.04` | **`1.19x`** | `17,900` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`34.89`** | `34.70` | `33.55` | **`1.040x`** | `0.937x` | `26.62` | **`1.31x`** | `17,868` | `fc7490e2b80c68e2` ✓ |

---

## 05d_duzeltme_wal_cokme — `performans-A@3707ece`

**Adım 5d — doğruluk düzeltmesi: WAL çökme atomikliği (performans amacı yok).** 5c envanterinde bulundu; WAL yazıcısı port edildiğinden beri vardı. Büyük bir yazma işleminde sayfa önbelleği taşıp sayfalar commit'ten önce `-wal`e yazıldığında ve taşmış bir sayfa yeniden değiştiğinde, `COMMIT` eski çerçeveyi yerinde yeniden yazar ve sonuna (commit çerçevesi dahil) yeni çerçeveler ekler. C bu sırada eklenen çerçeveleri tuz ve sağlama toplamı yerine 16 sıfır baytla yazar (`walEncodeFrame`, `iReCksum != 0`); geçerli zinciri ancak `walRewriteChecksums` kurar. Motor bu çerçeveleri gerçek tuz ve eski görüntüler üzerinden zincirlenmiş sağlama toplamıyla yazıyordu: çökme, eklenen çerçeveleri koruyup yerinde yeniden yazımı kaybederse **yarım uygulanmış bir işlem** görünür oluyordu (C aynı durumda işlemi atar). Artık C ile aynı: bekleyen yeniden yazım varken eklenen çerçeveler doğrulanamaz biçimde yazılıyor. Yazma sırası ve adım 1'deki çerçeve toplama değişmedi.

- **Testler**: `cargo test --workspace`: 546 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Dört yeni test, senkronlanmamış `-wal` yazımlarının her alt kümesini (test başına ~1.000–4.100 çökme görüntüsü) dener; kurtarma ya işlem öncesi durumu ya da işlemin tamamını vermeli. Düzeltme geri alınınca dördü de düşüyor. Süreç öldürme + blok geri yükleme betiği: düzeltmeden önce 600 satır ve iki güncellemeden biri uygulanmış; sonra C yazıcısıyla aynı sonuç (işlem atılıyor). **Hız:** toplam 0,999x; ölçüm iş yükleri önbellek taşırmadığı için `W13`/`W01` komut ve sistem çağrısı sayıları aynı. Doğrulanmayan: gerçek güç kesintisi (tüm çökme durumları sentetik). Kod okunarak bulunan ve düzeltilmeyen iki ara-durum farkı daha var (`journal_size_limit` kırpma hatası; `load_deserialized_image` içinde senkronsuz kapanış checkpoint'i).
- **Toplam**: `246.47 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.815x`** (disk WAL `0.787x`, bellek-içi `0.927x`)
- **Tepe RSS**: `17,964 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.50`** | `7.44` | `7.51` | **`0.999x`** | `0.973x` | `9.64` | **`0.78x`** | `8,856` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.57`** | `7.45` | `7.46` | **`1.016x`** | `0.813x` | `8.67` | **`0.87x`** | `9,696` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.21`** | `27.68` | `27.78` | **`1.016x`** | `0.764x` | `23.70` | **`1.19x`** | `9,696` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.87`** | `7.80` | `7.84` | **`1.003x`** | `0.546x` | `7.51` | **`1.05x`** | `9,760` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.11`** | `4.09` | `4.10` | **`1.003x`** | `0.239x` | `16.58` | **`0.25x`** | `9,760` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.81`** | `10.78` | `10.66` | **`1.014x`** | `0.995x` | `28.23` | **`0.38x`** | `9,952` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.61`** | `38.27` | `38.37` | **`1.006x`** | `0.990x` | `50.25` | **`0.77x`** | `12,432` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.36`** | `9.34` | `9.39` | **`0.996x`** | `0.534x` | `8.03` | **`1.17x`** | `12,432` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.95`** | `11.91` | `11.96` | **`0.999x`** | `0.998x` | `18.88` | **`0.63x`** | `12,432` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.23`** | `11.01` | `11.09` | **`1.013x`** | `0.366x` | `10.94` | **`1.03x`** | `12,616` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.25`** | `44.58` | `45.24` | **`1.000x`** | `0.953x` | `57.42` | **`0.79x`** | `17,740` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.06`** | `23.08` | `23.37` | **`1.029x`** | `0.590x` | `24.47` | **`0.98x`** | `17,780` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.84`** | `4.57` | `4.79` | **`1.011x`** | `0.671x` | `4.04` | **`1.20x`** | `17,812` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.10`** | `34.65` | `34.89` | **`1.006x`** | `0.942x` | `26.62` | **`1.32x`** | `17,780` | `fc7490e2b80c68e2` ✓ |

---

## 05e_duzeltme_guvence — `performans-A@e53008f`

**Adım 5e — doğruluk düzeltmeleri: güvence ve uyumluluk hataları (performans amacı yok).** (1) `PRAGMA secure_delete` yok sayılıyordu (satır dönmüyor, silinen hücrelerin içeriği dosyada kalıyordu); tüm biçimleriyle uygulandı, `ON` iken dosyada yalnızca canlı satırların baytları kalıyor. (2) C'de satır dönen ama burada dönmeyen pragmalar: `analysis_limit` (saklanıyor ve `ANALYZE` tarafından uygulanıyor), `data_version`, `cell_size_check` (yüklenen sayfalar C'deki gibi doğrulanıyor), `mmap_size`. (3) `ALTER TABLE ADD COLUMN` sütun metnini AST'den yeniden üretiyordu: `DEFAULT x'00'` → `x''` (varsayılan değer **kayboluyordu**), sabit olmayan varsayılan kabul ediliyordu; artık kullanıcının metni C'deki gibi aynen ekleniyor, C'nin kontrolleri C'nin sırası ve mesajlarıyla çalışıyor. (4) Tablosu olmayan veritabanının başlığı: şema biçimi ve kodlama ilk şema satırına kadar 0 kalıyor ve `PRAGMA encoding` o ana kadar geçerli. (5) İki WAL hata yolu: `journal_size_limit` kırpma hatası artık commit çerçevesi kalıcı olmuş bir `COMMIT`i başarısız yapmıyor; `load_deserialized_image` WAL'i senkronlu checkpoint ile kapatıyor.

- **Testler**: `cargo test --workspace`: 559 geçti (13 yeni), bilinen 4 başarısız, 337 yok sayıldı. `ADD COLUMN` farksalı 1.126 durumda C ile eşit: 1.118 (önce 407). Ayrışma bulucu (192 senaryo): hiçbir senaryo daha kötü katmanda bitmiyor, 11'i iyileşti. 13 düzen senaryosu önceki commit ile bayt bayt aynı. **Hız:** toplam 0,998x; komut sayıları aynı (`W08`/`W09` +%0,05). `W05` +%5 aynı komut sayısıyla (kod yerleşimi). **Bilerek C'den farklı bırakılan (mevcut entegrasyon testleri eski davranışı doğruluyor):** `ADD COLUMN` satır kontrolü hatası `SQLITE_CONSTRAINT` dönüyor (C: `SQLITE_ERROR`); `too many columns on t` (C: `… on sqlite_altertab_t`). Doğrulanmayan: deserialize yolu çalışan bir C ikilisine karşı.
- **Toplam**: `245.69 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.814x`** (disk WAL `0.788x`, bellek-içi `0.917x`)
- **Tepe RSS**: `17,836 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.43`** | `7.37` | `7.50` | **`0.991x`** | `0.964x` | `9.64` | **`0.77x`** | `8,792` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.58`** | `7.48` | `7.57` | **`1.001x`** | `0.814x` | `8.67` | **`0.88x`** | `9,632` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.64`** | `27.45` | `28.21` | **`0.980x`** | `0.748x` | `23.70` | **`1.17x`** | `9,632` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.75`** | `7.67` | `7.87` | **`0.984x`** | `0.537x` | `7.51` | **`1.03x`** | `9,696` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.31`** | `4.30` | `4.11` | **`1.048x`** | `0.250x` | `16.58` | **`0.26x`** | `9,696` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.62`** | `10.59` | `10.81` | **`0.983x`** | `0.978x` | `28.23` | **`0.38x`** | `9,888` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.83`** | `38.66` | `38.61` | **`1.006x`** | `0.995x` | `50.25` | **`0.77x`** | `12,432` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.44`** | `9.38` | `9.36` | **`1.009x`** | `0.538x` | `8.03` | **`1.18x`** | `12,432` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.83`** | `11.78` | `11.95` | **`0.990x`** | `0.988x` | `18.88` | **`0.63x`** | `12,432` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.08`** | `11.05` | `11.23` | **`0.986x`** | `0.361x` | `10.94` | **`1.01x`** | `12,680` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.21`** | `43.99` | `45.25` | **`0.999x`** | `0.952x` | `57.42` | **`0.79x`** | `17,676` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.65`** | `23.25` | `24.06` | **`0.983x`** | `0.580x` | `24.47` | **`0.97x`** | `17,696` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.87`** | `4.53` | `4.84` | **`1.005x`** | `0.675x` | `4.04` | **`1.21x`** | `17,728` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.44`** | `35.04` | `35.10` | **`1.010x`** | `0.951x` | `26.62` | **`1.33x`** | `17,696` | `fc7490e2b80c68e2` ✓ |

---

## 05f_duzeltme_fts5 — `performans-A@e4cdaff`

**Adım 5f — doğruluk düzeltmesi: FTS5'in C ile iki yönlü uyumu (performans amacı yok).** Bu değişiklikten önce iki motor bir FTS5 tablosunu güvenle paylaşamıyordu (farksal koşuda 300 tohumun 300'ü en az bir yönde başarısızdı). Düzeltilenler: **silme bayrağı** (okuma: C'nin `UPDATE`inden sonra `MATCH` satır kaçırıyor, ardından `'optimize'` o kayıtları kalıcı siliyordu; yazma: aynı boşaltmada silinip yeniden eklenen satırın bayrağı kayboluyordu); **64 KiB'ı aşan yaprak** (C: bozuk); **`contentless_delete=1`** kayıt biçimi; **ayraçlar** (unicode61, ascii, porter, trigram C'den bayt düzeyinde port edildi — katlama/aksan/kök bulma farklıydı, C `checksum mismatch` veriyordu); **çakışma yan tümceleri** (`INSERT OR REPLACE` vb.); **`contentless_unindexed=1`, `locale=1`**; FTS5 içinde gerçel→metin çevrimi. Ayrıca sorgu dilbilgisi, `bm25`/`rank`, `highlight`, `snippet`, `fts5vocab`, özel komutlar ve hata metinleri C'ye getirildi. **Kapsam dışı (yapısal, ayrı karar):** segment birleştirme takvimi ve boşaltma zamanı, yaprak boyutu hedefi ve doclist indeksi, yapı çerezi.

- **Testler**: `cargo test --workspace`: 597 geçti (38 yeni; 34'ü C CLI'sinden üretilmiş, gömülü C gölge tablo görüntüleriyle), bilinen 4 başarısız, 337 yok sayıldı. Yeni 34 durumun 32'si önceki kaynakta düşüyor. Farksal koşu (rastgele yapılandırma, işlemler ve sorgular; Rust yazar / C yazar / sırayla yazar; dört motor-dosya birleşiminde bütünlük kontrolü): 3.600 tohum / 10.800 çalıştırma, 0 hata (önce 900 çalıştırmanın 863'ü). 60 tohum yalnızca `secure-delete=1` ile farklı; her biri C'nin kendi indeksini bozmasına dayanıyor (51'i yalnız C'nin yazdığı geçmişte). **Hız:** toplam 1,004x; `W11` +%2,6 (komut sayısı 248,20 M → 247,86 M; `bm25`in C ile birebir aynı olması için eşleşen satır başına daha çok bellek). Düzeltilmeyen: `fts5_locale()` yok; `LIMIT` ötesindeki satırlar için yardımcı fonksiyon değerlendirmesi; `%_config` sürümü bağlanırken denetlenmiyor.
- **Toplam**: `246.97 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.817x`** (disk WAL `0.790x`, bellek-içi `0.923x`)
- **Tepe RSS**: `18,240 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.46`** | `7.40` | `7.43` | **`1.004x`** | `0.967x` | `9.64` | **`0.77x`** | `9,028` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.58`** | `7.56` | `7.58` | **`1.000x`** | `0.814x` | `8.67` | **`0.88x`** | `9,868` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.79`** | `27.68` | `27.64` | **`1.005x`** | `0.752x` | `23.70` | **`1.17x`** | `9,868` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.79`** | `7.68` | `7.75` | **`1.006x`** | `0.540x` | `7.51` | **`1.04x`** | `9,932` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.41`** | `4.38` | `4.31` | **`1.022x`** | `0.256x` | `16.58` | **`0.27x`** | `9,932` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.47`** | `10.41` | `10.62` | **`0.985x`** | `0.963x` | `28.23` | **`0.37x`** | `10,064` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.86`** | `38.50` | `38.83` | **`1.001x`** | `0.996x` | `50.25` | **`0.77x`** | `12,544` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.42`** | `9.39` | `9.44` | **`0.998x`** | `0.537x` | `8.03` | **`1.17x`** | `12,544` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.79`** | `11.70` | `11.83` | **`0.997x`** | `0.984x` | `18.88` | **`0.62x`** | `12,544` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.10`** | `11.06` | `11.08` | **`1.002x`** | `0.362x` | `10.94` | **`1.01x`** | `12,728` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.18`** | `45.55` | `45.21` | **`1.021x`** | `0.973x` | `57.42` | **`0.80x`** | `17,832` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.62`** | `23.32` | `23.65` | **`0.999x`** | `0.579x` | `24.47` | **`0.96x`** | `17,852` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.86`** | `4.69` | `4.87` | **`0.998x`** | `0.674x` | `4.04` | **`1.21x`** | `17,884` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.64`** | `35.27` | `35.44` | **`1.006x`** | `0.957x` | `26.62` | **`1.34x`** | `17,852` | `fc7490e2b80c68e2` ✓ |

---

## 05g_hizalama_dosya — `performans-A@fc4f0ca`

**Adım 5g — C'ye hizalama: dosya düzeyi, bedelsiz sınıflar.** Aynı ifade dizisi için yazılan dosyayı C'nin yazdığına yaklaştırır. (1) **Değişim sayacı** 0'dan başlıyor ve WAL commit'inde C gibi sabit kalıyor. (2) **Şema çerezi** satır başına değil ifade başına artıyor; `PRAGMA schema_version` her ifadeden sonra C ile aynı. (3) **Satır sıraları**: sanal tablo ve gölge tablolarının `sqlite_schema` sırası, `ANALYZE`in `sqlite_stat1` sırası, `CREATE TABLE … UNIQUE` yer tutucu satır dizisi, `VACUUM` ve `DROP TABLE` sırası. (4) **`REAL` sütunda tam değerli sayı** C'nin kuralıyla tamsayı olarak saklanıyor (dosya 1,8 kat küçülüyor); okuma tarafı da düzeltildi (C'nin yazdığı dosyada bu değerler bazı yollarda `integer` dönüyordu). `jsonb_set` vb. metni C'nin eleman türüyle yazıyor. (5) **Serbest bırakılan sayfalar** artık günlüğe alınmıyor, sıfırlanmıyor ve yeniden yazılmıyor (`DROP TABLE` 6.260 → 13 yazma çağrısı; C 25); `secure_delete` `ON`/`FAST`/`OFF` sayfa kararları C ile aynı. (6) **Serbest liste sırası** (ilk gövdenin ilk yaprağı / en yakın yaprak) ve **satır başına yazma sırası** (önce indeksler, sonra tablo satırı; `UPDATE`te yalnızca C'nin yeniden yazdığı indeksler). (7) Günlük başlığı 512 bayt; kısmi checkpoint C'nin sayfa seçimi; yeni dosyalar 0644 ve yan dosyalar veritabanının iznini alıyor. İki ek bulgu düzeltildi: iki `UNIQUE` ihlalinde hata sırası C'ninki oldu.

- **Testler**: `cargo test --workspace`: 626 geçti (29 yeni; 19'u C'nin yazdığı dosyayla bayt eşitliği), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu (192 senaryo):** hiçbir senaryo daha kötü katmanda bitmiyor; **22'si artık C ile bayt bayt aynı (önce 0)**, 42'si yalnızca kullanılmayan baytlarda farklı. **Karar eşliği** (izli C derlemesi ile Rust karar kayıtlarının olay olay karşılaştırması, 234 betik): çerez 234/234, `REAL` saklama 234/234, JSONB 234/234, şema satırları 227/234, serbest bırakma 181/234, tahsis 191/234, yazma sırası 170/234 — son üçü ağaç şekline bağlı; uyuşmayan betikler kapsam dışı sınıfların (silmede yeniden dengeleme, indeks doluluğu, bölünmeler, FTS5/R-Tree yazıcıları) ifadelerinde ayrışıyor. Ayrıntı: `saglik/karar_esligi_5g.md`. **Çökme güvenliği:** 258 geri alma betiği ve commit sırasında öldürülen 2.592 kurtarma (iki yazıcı × iki kurtarıcı), 0 hata (tmpfs; gerçek güç kesintisi yok). **Hız:** toplam 0,998x; en büyük komut artışı `W10` +%0,96. Hizalanmayan/doğrulanmayan: serbest listeden alınan sayfa hâlâ okunup sıfırlanıyor; indeks taramasıyla `DELETE` ve var olan satır kimliğinde `REPLACE` yazma sırası; `FAST` modunda dosyadaki artık C'den biraz fazla (nedeni incelenmedi); `PRAGMA optimize` yok.
- **Toplam**: `247.71 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.819x`** (disk WAL `0.791x`, bellek-içi `0.928x`)
- **Tepe RSS**: `18,104 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.57`** | `7.55` | `7.46` | **`1.015x`** | `0.982x` | `9.64` | **`0.79x`** | `8,876` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.45` | `7.58` | **`0.990x`** | `0.806x` | `8.67` | **`0.87x`** | `9,844` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.00`** | `27.70` | `27.79` | **`1.008x`** | `0.758x` | `23.70` | **`1.18x`** | `9,844` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.84`** | `7.75` | `7.79` | **`1.006x`** | `0.544x` | `7.51` | **`1.04x`** | `9,972` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.38`** | `4.36` | `4.41` | **`0.994x`** | `0.254x` | `16.58` | **`0.26x`** | `9,972` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.51`** | `10.47` | `10.47` | **`1.004x`** | `0.967x` | `28.23` | **`0.37x`** | `10,036` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`39.08`** | `38.57` | `38.86` | **`1.006x`** | `1.002x` | `50.25` | **`0.78x`** | `12,296` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.45`** | `9.42` | `9.42` | **`1.003x`** | `0.539x` | `8.03` | **`1.18x`** | `12,424` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.92`** | `11.86` | `11.79` | **`1.011x`** | `0.995x` | `18.88` | **`0.63x`** | `12,424` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.17`** | `11.14` | `11.10` | **`1.006x`** | `0.364x` | `10.94` | **`1.02x`** | `12,608` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.41`** | `45.50` | `46.18` | **`1.005x`** | `0.978x` | `57.42` | **`0.81x`** | `17,940` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.72`** | `23.18` | `23.62` | **`1.004x`** | `0.582x` | `24.47` | **`0.97x`** | `17,968` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.87`** | `4.58` | `4.86` | **`1.003x`** | `0.676x` | `4.04` | **`1.21x`** | `18,000` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.28`** | `34.91` | `35.64` | **`0.990x`** | `0.947x` | `26.62` | **`1.33x`** | `17,984` | `fc7490e2b80c68e2` ✓ |

---

## 05h_hizalama_sql — `performans-A@a0cffa4`

**Adım 5h — C'ye hizalama: SQL düzeyi, bedelsiz sınıflar.** SQL üzerinden gözlenen sonuçları C ile eşitler. (A) **Gerçel ↔ metin**: C'nin `sqlite3FpDecode` (17 anlamlı basamak, gidiş-dönüş indirgemesi) ve `sqlite3AtoF` davranışı tüm motorda (`CAST`, `||`, TEXT yakınlığı, `quote`, JSON, CLI); `printf` C'nin `sqlite3_str_vappendf` portu. (B) **Fonksiyonlar**: `substr` (negatif başlangıç, BLOB BLOB kalır), `hex`/`octet_length` (saklanan kodlama), `replace`, `instr`, `trim`, `lower`/`upper`, `round`, `ln`/`log`, `quote`, `like`/`glob`; listede olup uygulanmamış `concat`, `concat_ws`, `unistr`, `unistr_quote` eklendi; `count(DISTINCT)` 1 ile 1.0'ı tek değer sayıyor; `generate_series` metin işlenenli kısıt. (C) **İfade anlambilimi**: R-Tree'ye `INSERT OR FAIL`; satır kimliği 2^63−1'de C'nin rastgele aday algoritması; `sqlite_schema`dan CTAS; `PRAGMA table_info` varsayılan metni; üretilmiş sütun kontrolleri. (D) **`ORDER BY`sız `GROUP BY`** artık C gibi anahtar sırasıyla dönüyor (ölçülen bedel: komut sayısı 1,0000–1,0002). (E) `integrity_check`in üç SQL düzeyi kontrolü daha. (F) **CLI**: `Parse error near line N` + işaretçi satırları, `Error near line N`, hata öncesi satırlar, çıkış kodları, çıktı modlarının biçimi. Onaylanan iki test güncellemesi: `ADD COLUMN` satır kontrolü hatası `SQLITE_ERROR`; `too many columns on sqlite_altertab_t`.

- **Testler**: `cargo test --workspace`: 655 geçti (29 yeni), bilinen 4 başarısız, 337 yok sayıldı. Farksal: 109.942 gerçel değer × 16 SQL yolu + 8 CLI modu (1.019.281 çıktı satırı) C ile fark 0 (önce 754.128); fonksiyon taraması 257.527 çağrıda fark 3.317 (önce 62.648) — kalanların 2.796'sı tarih/saat ailesi (`date.c` port edilmedi, `timediff` yok), 504'ü JSON ayrıntıları. Ayrışma bulucu (234 senaryo): hiçbiri kötüleşmedi; stdout C ile eşit 230 → 233, stderr 228 → 234. **Hız:** toplam 1,005x; komut sayıları aynı, yalnız `W14` +%1,4 (`integrity_check` daha çok kontrol yapıyor). `W06` +%5,6 süre, aynı komut sayısıyla (kod yerleşimi; komut önbelleği ıskası +%14). **Hizalanmayan:** sanal tablolarda `total_changes()`/`changes()` (C gölge tablo yazımlarını sayıyor); tarih/saat fonksiyonları; UTF-16 veritabanında BINARY metin sırası; `PRAGMA optimize`; `integrity_check`te kesin olmayan kayan nokta mesajı ve satırlardan sonra hata; `fts5_locale()`; CLI'de savunmacı kip ve hazırlama zamanı hatasının `Parse error` olarak sınıflanmadığı durumlar (ör. üretilmiş sütun hatası). CLI crate'inin kendi üç testi C'den farklı davranışı doğruladığı için (`-json` tek satır, CSV satır sonu/tırnaklama, çok satırlı dizgede CRLF) o davranışlar bırakıldı.
- **Toplam**: `248.53 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.821x`** (disk WAL `0.794x`, bellek-içi `0.926x`)
- **Tepe RSS**: `18,156 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.62`** | `7.58` | `7.57` | **`1.007x`** | `0.988x` | `9.64` | **`0.79x`** | `9,064` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.65`** | `7.59` | `7.51` | **`1.019x`** | `0.821x` | `8.67` | **`0.88x`** | `9,968` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.34`** | `27.79` | `28.00` | **`1.012x`** | `0.767x` | `23.70` | **`1.20x`** | `9,968` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.81`** | `7.73` | `7.84` | **`0.996x`** | `0.542x` | `7.51` | **`1.04x`** | `10,032` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.08`** | `4.07` | `4.38` | **`0.932x`** | `0.237x` | `16.58` | **`0.25x`** | `10,032` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`11.11`** | `11.04` | `10.51` | **`1.057x`** | `1.022x` | `28.23` | **`0.39x`** | `10,160` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.67`** | `38.48` | `39.08` | **`0.990x`** | `0.991x` | `50.25` | **`0.77x`** | `12,648` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.43`** | `9.40` | `9.45` | **`0.998x`** | `0.538x` | `8.03` | **`1.17x`** | `12,648` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.03`** | `11.98` | `11.92` | **`1.009x`** | `1.004x` | `18.88` | **`0.64x`** | `12,648` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.09`** | `11.07` | `11.17` | **`0.993x`** | `0.362x` | `10.94` | **`1.01x`** | `12,832` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.50`** | `44.95` | `46.41` | **`0.980x`** | `0.958x` | `57.42` | **`0.79x`** | `18,032` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.24`** | `23.36` | `23.72` | **`1.022x`** | `0.595x` | `24.47` | **`0.99x`** | `18,088` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.87`** | `4.62` | `4.87` | **`0.999x`** | `0.675x` | `4.04` | **`1.21x`** | `18,120` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.10`** | `35.73` | `35.28` | **`1.023x`** | `0.969x` | `26.62` | **`1.36x`** | `18,108` | `fc7490e2b80c68e2` ✓ |

---

## 05i_duzeltme_yedekleme — `performans-A@39deab5`

**Adım 5i — doğruluk düzeltmesi: yedekleme API'sinde bağlantı kilitleri (performans amacı yok).** `backup_concurrent_bidirectional_steps_never_deadlock` testi yük altında ara sıra düşüyor ya da takılıyordu (3.000 koşuda 11 hata, 11 takılma; taban `4ccdc64`te de aynı). Motorda kilitlenme yoktu; takılma, testin kendi `Barrier`ında bekleyen iş parçacığıydı. Asıl kusur: `enter_dual` iki bağlantı kapısını deneme-kilidiyle alıyordu ve bir bağlantı anlık kullanımdayken `Backup::init`/`step`/`finish` **C'nin hiç dönmediği** `SQLITE_BUSY` (“database connection is busy”) dönüyordu. C bu üç çağrıda iki bağlantının mutex'ini engelleyerek alır. Artık motor da bekliyor — iki kapıyı her zaman artan bağlantı kimliği sırasıyla (tek küresel sıra), bu yüzden bekleme döngüsü oluşamıyor. İki sızıntı da giderildi: reddedilen bir `finish`/`Blob::close` çağrısı tutamacı kalıcı olarak “kullanımda” bırakıyordu. Ayrıca C'ye hizalananlar: ilk adımdan önce `remaining()`/`pagecount()` 0 (önce −1); yedekleme sürerken kaynağı kapatma `SQLITE_BUSY`; farklı sayfa boyutlu dolu bellek-içi hedef `SQLITE_READONLY`; hedef işlem ilk adımdan itibaren EXCLUSIVE; kopya sonrası hedef şema çerezi.

- **Testler**: `cargo test --workspace`: 669 geçti (14 yeni), bilinen 4 başarısız, 337 yok sayıldı. Özgün test: düzeltme sonrası 4.000 koşu (48 paralel, 30 sn zaman aşımı) — 4.000 geçti, 0 takılma, 0 hata (ajanın ayrıca bildirdiği: 16.000/16.000). Kilit testlerinin 7'si de eski davranış geri konunca düşüyor. **Hız:** toplam 0,989x; `W03`/`W13`/`W01` komut oranları 1,0005 / 0,9999 / 1,0002. **C hakkında not:** aynı senaryo C 3.54.1'in kendisinde iki yönlü yedeklemede kilitleniyor (tek turlu 3.000 koşunun %6'sı; ABBA sırası) — motor bunu taklit etmiyor. **Hizalanmayan (sonraki adıma):** başarısız son adımdan sonra `finish()` (C adımın kodunu döner, motor Ok); salt okunur hedefte C `init`i kabul edip `step`te hata verir (motor `init`te); aynı hedefe ikinci yedekleme (C izin verir); C'nin yeniden başlatmadığı durumlarda yeniden başlatma (`remaining()` dizisi farklı).
- **Toplam**: `248.74 ms` (taban `328.37 ms`, C `294.97 ms`) — geomean `Rust/C` **`0.824x`** (disk WAL `0.797x`, bellek-içi `0.930x`)
- **Tepe RSS**: `18,308 KiB` (C: `6,256 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.59`** | `7.56` | `7.62` | **`0.995x`** | `0.984x` | `9.64` | **`0.79x`** | `9,052` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.74`** | `7.59` | `7.65` | **`1.011x`** | `0.831x` | `8.67` | **`0.89x`** | `9,892` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.91`** | `27.70` | `28.34` | **`0.985x`** | `0.756x` | `23.70` | **`1.18x`** | `9,892` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.93`** | `7.75` | `7.81` | **`1.016x`** | `0.550x` | `7.51` | **`1.06x`** | `9,956` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `4.08` | **`1.049x`** | `0.248x` | `16.58` | **`0.26x`** | `10,020` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.90`** | `10.87` | `11.11` | **`0.981x`** | `1.003x` | `28.23` | **`0.39x`** | `10,084` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.82`** | `38.38` | `38.67` | **`1.004x`** | `0.995x` | `50.25` | **`0.77x`** | `12,508` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.43`** | `9.40` | `9.43` | **`1.000x`** | `0.538x` | `8.03` | **`1.17x`** | `12,508` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.01`** | `11.83` | `12.03` | **`0.999x`** | `1.003x` | `18.88` | **`0.64x`** | `12,508` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.17`** | `11.11` | `11.09` | **`1.008x`** | `0.364x` | `10.94` | **`1.02x`** | `12,692` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.04`** | `45.72` | `45.50` | **`1.012x`** | `0.970x` | `57.42` | **`0.80x`** | `17,864` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.05`** | `23.36` | `24.24` | **`0.992x`** | `0.590x` | `24.47` | **`0.98x`** | `17,912` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.89`** | `4.63` | `4.87` | **`1.005x`** | `0.678x` | `4.04` | **`1.21x`** | `17,944` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.98`** | `35.77` | `36.10` | **`0.996x`** | `0.966x` | `26.62` | **`1.35x`** | `17,912` | `fc7490e2b80c68e2` ✓ |
