# `performans-A` — Saf Rust Optimizasyon Adımları Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-13T04:42:37+03:00`
- **Yöntem**: `speedbench` `W01`–`W14`; 1 ısınma + 9 sıralı (interleaved) deneme, medyan; süreçler `taskset -c 20` ile tek çekirdeğe sabit; veritabanı dosyaları `tmpfs (/run/user/1000/sqliters-perf)` üzerinde (fsync gürültüsü yok). Tüm aşamaların saklanmış ikilileri ve C referansı **aynı oturumda** ölçüldü.
- **Kısıtlar (tüm aşamalar)**: `#![forbid(unsafe_code)]`, sıfır dış bağımlılık, SWAR / SIMD / `asm!` yok, SQLite Format-3 ve WAL protokolü uyumu, C ile birebir `FNV-1a` çıktı özeti.

> **Dikkat — farklı donanım.** Üst dizindeki `00`–`03` anlık görüntüleri ve `benchmark_report.md` başka bir makinede (`AMD Ryzen 7 8845HS`) ölçülmüştür; bu dizindeki sayılar o tabloyla doğrudan karşılaştırılamaz. Buradaki `00_performans_taban` satırı, aynı `performans@4ccdc64` kodunun **bu makinedeki** ölçümüdür ve `performans-A` adımlarının bazıdır.
>
> **Ölçülmeyen.** `./sqltest` uçtan uca CLI komut seti (126 örnek) ve C `rss_probe` tabanlı çocuk süreç RSS ölçümü bu aşamalarda koşulmadı; JSON'larda `sqltest_cli_command_set` alanı `null`.

**Dosyalar**: her aşama için `NN_<aşama>.json` (deneme bazında süreler, RSS, özetler); `asamalar.json` (aşama tanımları, commit'ler, test sonuçları); `olc.py` (bu raporu üreten betik); `ek_resmi_run_speedbench_taban.json` (bazın `tools/run_speedbench.py` ile sabitlenmemiş, btrfs üzerindeki ilk resmi koşusu: Rust `723,80 ms` / C `647,26 ms`, geomean `1,12x` — fsync ve zamanlama gürültüsü içerir).

## 1. Aşama Özeti

| Aşama | Git (`dal@commit`) | İkili (`KiB`) | `.text` (`KiB`) | Toplam (`ms`) | Geomean (`vs taban`) | Geomean (`vs C`) | Toplam (`vs C`) | Tepe RSS (`KiB`) | Özetler |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`294.85`** | `—` | **`1.000x`** | **`1.000x`** | `6,160` | `—` |
| **00_performans_taban** | `performans@4ccdc64` | `6,382.7` | `5,412.9` | **`328.95`** | `1.000x` | **`1.186x`** | `1.116x` | `17,976` | 14/14 ✓ |
| **01_a1_wal_islem** | `performans-A@693ac8b` | `6,388.4` | `5,414.5` | **`313.56`** | `0.933x` | **`1.107x`** | `1.063x` | `18,012` | 14/14 ✓ |
| **02_a2_jsonb_indeks** | `performans-A@e2f7843` | `6,417.4` | `5,438.1` | **`287.96`** | `0.855x` | **`1.014x`** | `0.977x` | `17,672` | 14/14 ✓ |
| **03_a3_nokta_dml** | `performans-A@946f61e` | `6,456.6` | `5,472.7` | **`282.03`** | `0.820x` | **`0.972x`** | `0.957x` | `17,712` | 14/14 ✓ |
| **04_a4_kapsayan_indeks** | `performans-A@40e275d` | `6,644.6` | `5,647.8` | **`261.27`** | `0.709x` | **`0.841x`** | `0.886x` | `18,320` | 14/14 ✓ |
| **05_a5_rtree** | `performans-A@700ded4` | `6,650.1` | `5,652.7` | **`244.21`** | `0.683x` | **`0.810x`** | `0.828x` | `17,892` | 14/14 ✓ |
| **05b_duzeltme_dengesiz_agac** | `performans-A@1c12e74` | `6,717.4` | `5,713.6` | **`244.56`** | `0.681x` | **`0.808x`** | `0.829x` | `17,952` | 14/14 ✓ |
| **05d_duzeltme_wal_cokme** | `performans-A@3707ece` | `6,717.5` | `5,713.7` | **`244.45`** | `0.682x` | **`0.810x`** | `0.829x` | `17,864` | 14/14 ✓ |
| **05e_duzeltme_guvence** | `performans-A@e53008f` | `6,734.0` | `5,727.9` | **`244.40`** | `0.682x` | **`0.809x`** | `0.829x` | `17,888` | 14/14 ✓ |
| **05f_duzeltme_fts5** | `performans-A@e4cdaff` | `6,751.0` | `5,734.4` | **`246.46`** | `0.688x` | **`0.816x`** | `0.836x` | `18,104` | 14/14 ✓ |
| **05g_hizalama_dosya** | `performans-A@fc4f0ca` | `6,802.1` | `5,778.2` | **`245.92`** | `0.686x` | **`0.814x`** | `0.834x` | `18,144` | 14/14 ✓ |
| **05h_hizalama_sql** | `performans-A@a0cffa4` | `6,883.8` | `5,850.2` | **`247.92`** | `0.690x` | **`0.819x`** | `0.841x` | `18,244` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_taban | 01_a1_wal_islem | 02_a2_jsonb_indeks | 03_a3_nokta_dml | 04_a4_kapsayan_indeks | 05_a5_rtree | 05b_duzeltme_dengesiz_agac | 05d_duzeltme_wal_cokme | 05e_duzeltme_guvence | 05f_duzeltme_fts5 | 05g_hizalama_dosya | 05h_hizalama_sql |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.66` | `7.76` (**0.80x**) | `7.43` (**0.77x**) | `7.45` (**0.77x**) | `7.62` (**0.79x**) | `7.30` (**0.76x**) | `7.44` (**0.77x**) | `7.51` (**0.78x**) | `7.49` (**0.78x**) | `7.45` (**0.77x**) | `7.49` (**0.78x**) | `7.59` (**0.79x**) | `7.64` (**0.79x**) |
| `W02_create_indexes` | `8.68` | `9.34` (**1.08x**) | `9.02` (**1.04x**) | `8.51` (**0.98x**) | `7.51` (**0.86x**) | `7.49` (**0.86x**) | `7.67` (**0.88x**) | `7.43` (**0.86x**) | `7.52` (**0.87x**) | `7.57` (**0.87x**) | `7.58` (**0.87x**) | `7.51` (**0.86x**) | `7.64` (**0.88x**) |
| `W03_pk_point_select` | `23.25` | `36.92` (**1.59x**) | `27.75` (**1.19x**) | `27.60` (**1.19x**) | `28.00` (**1.20x**) | `27.54` (**1.18x**) | `28.02` (**1.21x**) | `27.96` (**1.20x**) | `27.90` (**1.20x**) | `27.43` (**1.18x**) | `27.60` (**1.19x**) | `27.74` (**1.19x**) | `27.79` (**1.20x**) |
| `W04_secondary_idx_range` | `7.46` | `14.34` (**1.92x**) | `12.54` (**1.68x**) | `12.52` (**1.68x**) | `12.71` (**1.70x**) | `7.75` (**1.04x**) | `7.85` (**1.05x**) | `7.83` (**1.05x**) | `7.87` (**1.06x**) | `7.66` (**1.03x**) | `7.80` (**1.05x**) | `7.82` (**1.05x**) | `7.72` (**1.03x**) |
| `W05_full_scan_filter_agg` | `16.61` | `17.24` (**1.04x**) | `17.83` (**1.07x**) | `17.52` (**1.05x**) | `18.04` (**1.09x**) | `4.39` (**0.27x**) | `4.31` (**0.26x**) | `4.10` (**0.25x**) | `4.12` (**0.25x**) | `4.30` (**0.26x**) | `4.41` (**0.27x**) | `4.37` (**0.26x**) | `4.09` (**0.25x**) |
| `W06_join_group_order` | `28.20` | `10.85` (**0.39x**) | `10.71` (**0.38x**) | `10.53` (**0.37x**) | `10.61` (**0.38x**) | `10.59` (**0.38x**) | `10.42` (**0.37x**) | `10.65` (**0.38x**) | `10.81` (**0.38x**) | `10.69` (**0.38x**) | `10.50` (**0.37x**) | `10.52` (**0.37x**) | `11.07` (**0.39x**) |
| `W07_window_functions` | `50.15` | `38.97` (**0.78x**) | `38.60` (**0.77x**) | `38.43` (**0.77x**) | `38.91` (**0.78x**) | `38.41` (**0.77x**) | `38.26` (**0.76x**) | `38.34` (**0.76x**) | `38.30` (**0.76x**) | `38.78` (**0.77x**) | `38.55` (**0.77x**) | `38.76` (**0.77x**) | `38.38` (**0.77x**) |
| `W08_indexed_update_delete` | `8.06` | `17.60` (**2.18x**) | `16.39` (**2.03x**) | `15.74` (**1.95x**) | `9.41` (**1.17x**) | `9.53` (**1.18x**) | `9.43` (**1.17x**) | `9.40` (**1.17x**) | `9.38` (**1.16x**) | `9.40` (**1.17x**) | `9.41` (**1.17x**) | `9.45` (**1.17x**) | `9.44` (**1.17x**) |
| `W09_like_text_scan` | `18.90` | `12.12` (**0.64x**) | `11.98` (**0.63x**) | `11.88` (**0.63x**) | `11.86` (**0.63x**) | `11.77` (**0.62x**) | `11.79` (**0.62x**) | `11.95` (**0.63x**) | `11.90` (**0.63x**) | `11.83` (**0.63x**) | `11.97` (**0.63x**) | `11.88` (**0.63x**) | `12.02` (**0.64x**) |
| `W10_jsonb_extract_agg` | `10.96` | `30.72` (**2.80x**) | `30.70` (**2.80x**) | `11.59` (**1.06x**) | `11.20` (**1.02x**) | `10.94` (**1.00x**) | `11.02` (**1.00x**) | `11.12` (**1.01x**) | `11.14` (**1.02x**) | `11.11` (**1.01x**) | `11.14` (**1.02x**) | `11.13` (**1.02x**) | `11.11` (**1.01x**) |
| `W11_fts5_index_and_bm25` | `57.74` | `48.34` (**0.84x**) | `48.99` (**0.85x**) | `46.94` (**0.81x**) | `47.13` (**0.82x**) | `47.53` (**0.82x**) | `45.97` (**0.80x**) | `45.18` (**0.78x**) | `44.90` (**0.78x**) | `44.84` (**0.78x**) | `45.94` (**0.80x**) | `45.95` (**0.80x**) | `45.64` (**0.79x**) |
| `W12_rtree_insert_and_bbox` | `24.43` | `40.20` (**1.65x**) | `40.25` (**1.65x**) | `40.55` (**1.66x**) | `40.43` (**1.66x**) | `39.97` (**1.64x**) | `23.73` (**0.97x**) | `23.43` (**0.96x**) | `23.39` (**0.96x**) | `23.63` (**0.97x**) | `23.65` (**0.97x**) | `23.39` (**0.96x**) | `24.59` (**1.01x**) |
| `W13_autocommit_wal_txns` | `4.08` | `7.21` (**1.77x**) | `4.91` (**1.21x**) | `4.93` (**1.21x**) | `4.84` (**1.19x**) | `4.77` (**1.17x**) | `4.77` (**1.17x**) | `4.68` (**1.15x**) | `4.72` (**1.16x**) | `4.62` (**1.13x**) | `4.85` (**1.19x**) | `4.73` (**1.16x**) | `4.85` (**1.19x**) |
| `W14_cli_sql_script` | `26.66` | `37.33` (**1.40x**) | `36.48` (**1.37x**) | `33.77` (**1.27x**) | `33.76` (**1.27x**) | `33.27` (**1.25x**) | `33.52` (**1.26x**) | `34.98` (**1.31x**) | `35.02` (**1.31x**) | `35.09` (**1.32x**) | `35.56` (**1.33x**) | `35.06` (**1.31x**) | `35.94` (**1.35x**) |
| **Toplam** | **`294.85`** | **`328.95`** (**1.12x**) | **`313.56`** (**1.06x**) | **`287.96`** (**0.98x**) | **`282.03`** (**0.96x**) | **`261.27`** (**0.89x**) | **`244.21`** (**0.83x**) | **`244.56`** (**0.83x**) | **`244.45`** (**0.83x**) | **`244.40`** (**0.83x**) | **`246.46`** (**0.84x**) | **`245.92`** (**0.83x**) | **`247.92`** (**0.84x**) |

---

## 00_performans_taban — `performans@4ccdc64`

**Baz.** `performans` dalının (Saf Rust tavanı, P1-C) bu makinedeki ölçümü; `performans-A` adımlarının karşılaştırma noktası.

- **Testler**: `cargo test --workspace`: 517 geçti, 4 başarısız, 337 yok sayıldı. Başarısız 4 test (`tests/phase7_vtab_framework_and_pragma_tables.rs` içindeki `*_differential_parity`) referans ikili için başka bir makinenin yolunu sabit kodluyor; bu makinede tüm aşamalarda aynı şekilde düşüyor. (C uyumluluk testleri için `sqlite3` 3.54.1 referans ikilisi `PATH`e eklenerek koşuldu.)
- **Toplam**: `328.95 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`1.186x`** (disk WAL `1.099x`, bellek-içi `1.568x`)
- **Tepe RSS**: `17,976 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.76`** | `7.68` | `1.000x` | `9.66` | **`0.80x`** | `8,680` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.34`** | `9.26` | `1.000x` | `8.68` | **`1.08x`** | `9,652` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`36.92`** | `36.52` | `1.000x` | `23.25` | **`1.59x`** | `9,652` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`14.34`** | `14.21` | `1.000x` | `7.46` | **`1.92x`** | `9,716` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.24`** | `17.22` | `1.000x` | `16.61` | **`1.04x`** | `9,716` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.85`** | `10.83` | `1.000x` | `28.20` | **`0.39x`** | `9,780` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.97`** | `38.66` | `1.000x` | `50.15` | **`0.78x`** | `12,228` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`17.60`** | `17.31` | `1.000x` | `8.06` | **`2.18x`** | `12,292` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.12`** | `11.92` | `1.000x` | `18.90` | **`0.64x`** | `12,292` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.72`** | `30.53` | `1.000x` | `10.96` | **`2.80x`** | `13,328` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`48.34`** | `46.17` | `1.000x` | `57.74` | **`0.84x`** | `17,892` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.20`** | `39.83` | `1.000x` | `24.43` | **`1.65x`** | `17,944` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`7.21`** | `7.07` | `1.000x` | `4.08` | **`1.77x`** | `17,976` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`37.33`** | `36.88` | `1.000x` | `26.66` | **`1.40x`** | `17,944` | `fc7490e2b80c68e2` ✓ |

---

## 01_a1_wal_islem — `performans-A@693ac8b`

**Adım 1 — WAL okuma/yazma işlemi açılış-kapanış maliyeti.** Sistem çağrısı sayısı zaten C ile aynıydı; fark kullanıcı alanındaydı (okuma işlemi başına ~32 mutex turu, tek tek doğrulanan ~70 shm kelime okuması). `validate_offset` satır-içi tek koşul oldu (ayrıntılı hata yolu soğuk fonksiyonda aynen duruyor); bağlantı başına bölge-0 önbelleği, kilitsiz kapalı/zehirli bayrağı, birleşik kilit-sonrası doğrulama (aynı kontroller, aynı sıra); bayt sırasına göre kelime döngülü WAL sağlama toplamı; ardışık WAL çerçeveleri tek konumlu yazmada (≤ 64 KiB) toplanıyor. Hedef: `W03`, `W13`, `W04`.

- **Testler**: `cargo test --workspace`: 517 geçti, bilinen 4 başarısız, 337 yok sayıldı. Windows arka ucu derlenmedi (koda dokunulmadı; bölge-0 önbelleğinin dayandığı değişmez orada yalnızca kod okunarak kontrol edildi).
- **Toplam**: `313.56 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`1.107x`** (disk WAL `1.006x`, bellek-içi `1.575x`)
- **Tepe RSS**: `18,012 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.43`** | `7.35` | `7.76` | **`0.957x`** | `0.957x` | `9.66` | **`0.77x`** | `8,588` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.02`** | `8.96` | `9.34` | **`0.965x`** | `0.965x` | `8.68` | **`1.04x`** | `9,420` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.75`** | `27.44` | `36.92` | **`0.752x`** | `0.752x` | `23.25` | **`1.19x`** | `9,420` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.54`** | `12.45` | `14.34` | **`0.874x`** | `0.874x` | `7.46` | **`1.68x`** | `9,484` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.83`** | `17.66` | `17.24` | **`1.034x`** | `1.034x` | `16.61` | **`1.07x`** | `9,484` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.71`** | `10.68` | `10.85` | **`0.987x`** | `0.987x` | `28.20` | **`0.38x`** | `9,548` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.60`** | `38.34` | `38.97` | **`0.991x`** | `0.991x` | `50.15` | **`0.77x`** | `11,908` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`16.39`** | `16.30` | `17.60` | **`0.931x`** | `0.931x` | `8.06` | **`2.03x`** | `11,908` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.98`** | `11.69` | `12.12` | **`0.988x`** | `0.988x` | `18.90` | **`0.63x`** | `11,908` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.70`** | `30.59` | `30.72` | **`0.999x`** | `0.999x` | `10.96` | **`2.80x`** | `12,944` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`48.99`** | `46.44` | `48.34` | **`1.013x`** | `1.013x` | `57.74` | **`0.85x`** | `17,720` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.25`** | `39.81` | `40.20` | **`1.001x`** | `1.001x` | `24.43` | **`1.65x`** | `17,796` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.91`** | `4.57` | `7.21` | **`0.681x`** | `0.681x` | `4.08` | **`1.21x`** | `17,828` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.48`** | `36.15` | `37.33` | **`0.977x`** | `0.977x` | `26.66` | **`1.37x`** | `17,796` | `fc7490e2b80c68e2` ✓ |

---

## 02_a2_jsonb_indeks — `performans-A@e2f7843`

**Adım 2 — ham indeks yaprağı yeniden dengeleme ve indeks sıralı akışlı kümeleme.** `W10`daki fark JSON'dan değil iki genel yoldan geliyordu: (1) dolu indeks yaprağına her eklemede üst düğüm + 3 kardeş yaprak hücre hücre `Vec`e ayrıştırılıp yeniden yazılıyordu; `try_insert_index_balance_raw` aynı dağıtımı ham sayfa baytları üzerinde yapıyor (sayfa çıktısı eski yolla bayt bayt aynı, desteklemediği her durumda sayfaya dokunmadan eski yola düşüyor). (2) Akışlı kümeleme planları kapsayıcı olmayan indeks taramasını reddediyordu; yeni `IndexOrderedTableLookup` erişim yolu eklendi. Ayrıca kayıt karşılaştırmada tamsayı-tamsayı hızlı yolu. **Davranış düzeltmesi:** `HashGroupByPlan` tamsayı `1` ile gerçel `1.0` anahtarlarını iki ayrı grup sayıyordu; artık SQLite'taki gibi tek grup. Hedef: `W10`; yan kazanç `W02`, `W08`, `W14`.

- **Testler**: `cargo test --workspace`: 520 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. 9.000 satırlık rastgele indeksli yükte yazılan veritabanı baz ikilininkiyle bayt bayt aynı ve C `integrity_check` geçiyor. Sayfa başına ayrılmış bayt (reserved bytes) kullanan veritabanlarında denenmedi.
- **Toplam**: `287.96 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`1.014x`** (disk WAL `0.986x`, bellek-içi `1.125x`)
- **Tepe RSS**: `17,672 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.45`** | `7.37` | `7.43` | **`1.003x`** | `0.960x` | `9.66` | **`0.77x`** | `8,572` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`8.51`** | `8.43` | `9.02` | **`0.944x`** | `0.911x` | `8.68` | **`0.98x`** | `9,404` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.60`** | `27.48` | `27.75` | **`0.995x`** | `0.748x` | `23.25` | **`1.19x`** | `9,404` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.52`** | `12.46` | `12.54` | **`0.999x`** | `0.874x` | `7.46` | **`1.68x`** | `9,468` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.52`** | `17.46` | `17.83` | **`0.983x`** | `1.016x` | `16.61` | **`1.05x`** | `9,532` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.53`** | `10.49` | `10.71` | **`0.984x`** | `0.971x` | `28.20` | **`0.37x`** | `9,596` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.43`** | `37.95` | `38.60` | **`0.996x`** | `0.986x` | `50.15` | **`0.77x`** | `13,064` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`15.74`** | `15.56` | `16.39` | **`0.961x`** | `0.894x` | `8.06` | **`1.95x`** | `13,064` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.88`** | `11.83` | `11.98` | **`0.992x`** | `0.980x` | `18.90` | **`0.63x`** | `13,064` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.59`** | `11.50` | `30.70` | **`0.377x`** | `0.377x` | `10.96` | **`1.06x`** | `13,032` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.94`** | `46.00` | `48.99` | **`0.958x`** | `0.971x` | `57.74` | **`0.81x`** | `17,416` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.55`** | `40.07` | `40.25` | **`1.007x`** | `1.009x` | `24.43` | **`1.66x`** | `17,492` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.93`** | `4.58` | `4.91` | **`1.004x`** | `0.684x` | `4.08` | **`1.21x`** | `17,524` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.77`** | `33.63` | `36.48` | **`0.926x`** | `0.905x` | `26.66` | **`1.27x`** | `17,492` | `fc7490e2b80c68e2` ✓ |

---

## 03_a3_nokta_dml — `performans-A@946f61e`

**Adım 3 — satır kimliğiyle `UPDATE`/`DELETE` için özel planlar.** Hazırlanmış `UPDATE … WHERE rowid = ?` ve `DELETE … WHERE rowid = ?` genel VDBE yorumlayıcısından geçiyor, satır başına ikinci bir B-tree inişi ve tam kayıt çöz/yeniden kodla yapıyordu. `RowidPointUpdatePlan` / `RowidPointDeletePlan` tek ham arama yapıp değişmeyen alanları ham bayt olarak kopyalıyor; aynı indeks sil/ekle ve satır değiştir çağrılarını aynı baytlarla yapıyor. Yalnızca düz `main` tabloları için (tetikleyici, yabancı anahtar, `CHECK`, `STRICT`, üretilmiş sütun, kısmi/ifade indeksi, özel harmanlama, çakışma yan tümcesi, `RETURNING` yok); diğer her durumda VDBE programı çalışıyor. Ek olarak konumlu tablo işlemleri, ham yaprak yeniden paketleme ve bir kez çözülen indeks arama anahtarı (`RawRecordKey`). Hedef: `W08`; yan kazanç `W02`.

- **Testler**: `cargo test --workspace`: 524 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Rastgele farksal koşu (1.900 tohum, ~2,1 milyon ifade): çıktı, hata kodları, değişiklik sayaçları ve veritabanı baytları adım 2 ile aynı; tüm veritabanları C `integrity_check` geçiyor. Plan ortasında gelen G/Ç hatası / meşgul / kesme durumları hata enjeksiyonuyla denenmedi.
- **Toplam**: `282.03 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.972x`** (disk WAL `0.937x`, bellek-içi `1.113x`)
- **Tepe RSS**: `17,712 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.62`** | `7.46` | `7.45` | **`1.023x`** | `0.983x` | `9.66` | **`0.79x`** | `8,660` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.43` | `8.51` | **`0.883x`** | `0.805x` | `8.68` | **`0.86x`** | `9,500` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.00`** | `27.35` | `27.60` | **`1.014x`** | `0.758x` | `23.25` | **`1.20x`** | `9,500` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.71`** | `12.56` | `12.52` | **`1.015x`** | `0.886x` | `7.46` | **`1.70x`** | `9,564` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`18.04`** | `17.98` | `17.52` | **`1.030x`** | `1.047x` | `16.61` | **`1.09x`** | `9,564` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.61`** | `10.59` | `10.53` | **`1.008x`** | `0.978x` | `28.20` | **`0.38x`** | `9,692` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.91`** | `38.38` | `38.43` | **`1.012x`** | `0.998x` | `50.15` | **`0.78x`** | `11,872` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.41`** | `9.36` | `15.74` | **`0.598x`** | `0.535x` | `8.06` | **`1.17x`** | `11,872` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.86`** | `11.67` | `11.88` | **`0.998x`** | `0.978x` | `18.90` | **`0.63x`** | `11,872` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.20`** | `11.12` | `11.59` | **`0.967x`** | `0.365x` | `10.96` | **`1.02x`** | `12,056` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.13`** | `46.32` | `46.94` | **`1.004x`** | `0.975x` | `57.74` | **`0.82x`** | `17,364` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.43`** | `39.41` | `40.55` | **`0.997x`** | `1.006x` | `24.43` | **`1.66x`** | `17,440` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.84`** | `4.53` | `4.93` | **`0.981x`** | `0.671x` | `4.08` | **`1.19x`** | `17,472` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.76`** | `33.29` | `33.77` | **`1.000x`** | `0.904x` | `26.66` | **`1.27x`** | `17,440` | `fc7490e2b80c68e2` ✓ |

---

## 04_a4_kapsayan_indeks — `performans-A@40e275d`

**Adım 4 — kapsayan indeks taramalarında tamsayı çekirdeği.** `W04` ve `W05` zamanlarını hücre başına genel işte harcıyordu: ziyaret edilen her hücre için tam kayıt-aralık karşılaştırması, satır başına genel başlık ayrıştırma ve enum dağıtımlı `WHERE`/kümeleme değerlendirmesi. `IntRangeProbe` aralık hedefini arama başına bir kez çözüyor (tüm işlenenler `INTEGER` ve `DESC` sütun yoksa); `RawIndexLeafView` sayfa başına değişmezleri döngü dışına alıyor; `CovIntKernelShape` (`fast_read/int_kernel.rs`), `WHERE` sütun karşılaştırmaları/`BETWEEN` ve kümeler `count/sum/total/avg/min/max` olduğunda satırları tamsayı aralık testi + mevcut `FastSumAccum` ile işliyor (taşma anlambilimi aynı). Çekirdeğin çözemediği her satır ve tamsayı olmayan parametreli her yürütme mevcut genel koddan geçiyor. Hedef: `W04`, `W05`.

- **Testler**: `cargo test --workspace`: 527 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. Farksal koşular: 2.000 tohum (400 bin sorgu, yarısı bozuk indeks sayfalarıyla) ve 700 rastgele aralık/kümeleme sorgusu adım 3 ile birebir aynı; ikincisi ondalık biçimi normalleştirilince C ile de aynı. `W11` (+%3) ve `W09` (+%2,5) aynı komut sayısıyla yavaş okunuyor (kod yerleşimi; bu iş yüklerinin koduna dokunulmadı).
- **Toplam**: `261.27 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.841x`** (disk WAL `0.781x`, bellek-içi `1.103x`)
- **Tepe RSS**: `18,320 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.30`** | `7.24` | `7.62` | **`0.958x`** | `0.942x` | `9.66` | **`0.76x`** | `8,692` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.49`** | `7.42` | `7.51` | **`0.997x`** | `0.802x` | `8.68` | **`0.86x`** | `9,528` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.54`** | `27.38` | `28.00` | **`0.984x`** | `0.746x` | `23.25` | **`1.18x`** | `9,528` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.75`** | `7.63` | `12.71` | **`0.610x`** | `0.540x` | `7.46` | **`1.04x`** | `9,592` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.39`** | `4.37` | `18.04` | **`0.244x`** | `0.255x` | `16.61` | **`0.27x`** | `9,592` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.59`** | `10.56` | `10.61` | **`0.998x`** | `0.976x` | `28.20` | **`0.38x`** | `9,788` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.41`** | `38.07` | `38.91` | **`0.987x`** | `0.986x` | `50.15` | **`0.77x`** | `13,244` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.53`** | `9.44` | `9.41` | **`1.013x`** | `0.542x` | `8.06` | **`1.18x`** | `13,244` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.77`** | `11.73` | `11.86` | **`0.992x`** | `0.971x` | `18.90` | **`0.62x`** | `13,244` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.94`** | `10.91` | `11.20` | **`0.976x`** | `0.356x` | `10.96` | **`1.00x`** | `13,212` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.53`** | `46.16` | `47.13` | **`1.009x`** | `0.983x` | `57.74` | **`0.82x`** | `17,912` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`39.97`** | `39.38` | `40.43` | **`0.989x`** | `0.994x` | `24.43` | **`1.64x`** | `18,000` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.77`** | `4.53` | `4.84` | **`0.987x`** | `0.662x` | `4.08` | **`1.17x`** | `18,032` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.27`** | `33.03` | `33.76` | **`0.986x`** | `0.891x` | `26.66` | **`1.25x`** | `18,000` | `fc7490e2b80c68e2` ✓ |

---

## 05_a5_rtree — `performans-A@700ded4`

**Adım 5 — R-Tree yerinde düğüm güncelleme ve sanal tablo işlem sırasının belirlenimci yapılması.** `W12`nin süresi disk G/Ç'si değil CPU işiydi: bölünmeyen eklemelerde ve üst düğüm sınır kutusu düzeltmesinde düğüm `klonla → değiştir → geri koy` yerine yerinde değiştiriliyor, tam düğüm anlık görüntüsü yerine küçük geri alma kayıtları tutuluyor; düğüm sınır kutusu önbelleği, tek sorgulu önbellek erişimi, düğüm numaralı haritalar için ucuz hasher; `SELECT`in `WHERE` ifadesi yürütme başına 4 kez derin kopyalanıyordu, artık ödünç alınıyor. Gölge tablo baytları değişmedi. **Belirlenimsizlik düzeltmesi (tabanda da vardı, doğrulama sırasında bulundu):** aynı işlemde iki veya daha çok sanal tablo değişince dosya, süreç başına rastgele seçilen birkaç bayt-farklı varyanttan biri oluyordu (işlem yayılımı rastgele tohumlu `HashMap` sırasıyla yürüyordu). Artık tablolar işleme katılma sırasıyla işleniyor (C'deki `db->aVTrans` sırası). Mantıksal içerik hiç etkilenmiyordu; fark yalnızca gölge tablolar arasındaki sayfa tahsis sırasındaydı. Hedef: `W12`; yan kazanç `W11`.

- **Testler**: `cargo test --workspace`: 533 geçti (6 yeni), bilinen 4 başarısız, 337 yok sayıldı. Çok sanal tablolu iki tekrar betiği 16'şar koşuda tek dosya üretiyor (önceden iki); çıktı adım 4 ile aynı, C `rtreecheck` + `integrity_check` `ok`. Farksal koşu: 650 tek R-Tree tohumu çıktı ve dosya baytlarında adım 4 ile aynı; 300 çok R-Tree tohumu çıktıda aynı ve koşudan koşuya kendisiyle aynı. Dokunulmayan bilinen hatalar (tabanda var): 512 bayt sayfalı bazı R-Tree dosyalarında C `integrity_check` → `Child page depth differs`; R-Tree satır kimliği tahsisi `i64::MAX`ta taşıyor. Ayrıntı: `saglik/c_uyumluluk_ve_saglik_analizi.md`.
- **Toplam**: `244.21 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.810x`** (disk WAL `0.783x`, bellek-içi `0.919x`)
- **Tepe RSS**: `17,892 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.44`** | `7.39` | `7.30` | **`1.019x`** | `0.959x` | `9.66` | **`0.77x`** | `8,636` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.67`** | `7.58` | `7.49` | **`1.024x`** | `0.822x` | `8.68` | **`0.88x`** | `9,476` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.02`** | `27.52` | `27.54` | **`1.017x`** | `0.759x` | `23.25` | **`1.21x`** | `9,476` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.85`** | `7.73` | `7.75` | **`1.014x`** | `0.548x` | `7.46` | **`1.05x`** | `9,540` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.31`** | `4.30` | `4.39` | **`0.981x`** | `0.250x` | `16.61` | **`0.26x`** | `9,604` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.42`** | `10.35` | `10.59` | **`0.983x`** | `0.960x` | `28.20` | **`0.37x`** | `9,732` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.26`** | `38.05` | `38.41` | **`0.996x`** | `0.982x` | `50.15` | **`0.76x`** | `12,212` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.43`** | `9.35` | `9.53` | **`0.989x`** | `0.536x` | `8.06` | **`1.17x`** | `12,212` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.79`** | `11.74` | `11.77` | **`1.002x`** | `0.972x` | `18.90` | **`0.62x`** | `12,212` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.02`** | `10.90` | `10.94` | **`1.007x`** | `0.359x` | `10.96` | **`1.00x`** | `12,460` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.97`** | `44.35` | `47.53` | **`0.967x`** | `0.951x` | `57.74` | **`0.80x`** | `17,544` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.73`** | `22.94` | `39.97` | **`0.594x`** | `0.590x` | `24.43` | **`0.97x`** | `17,544` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.77`** | `4.54` | `4.77` | **`0.999x`** | `0.661x` | `4.08` | **`1.17x`** | `17,576` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.52`** | `33.06` | `33.27` | **`1.008x`** | `0.898x` | `26.66` | **`1.26x`** | `17,544` | `fc7490e2b80c68e2` ✓ |

---

## 05b_duzeltme_dengesiz_agac — `performans-A@1c12e74`

**Adım 5b — doğruluk düzeltmesi (performans amacı yok).** Üç hata da ilk port commit'inden (`814e41e`) beri vardı. (1) **Dengesiz tablo B-tree'si:** silme, kök olmayan bir iç sayfayı tek çocukla bırakınca o sayfa serbest bırakılıp üst düğüm doğrudan toruna bağlanıyordu; alt ağaç kardeşlerinden bir seviye kısa kalıyordu. C bunu bozulma sayıyor (`Child page depth differs`), motorun kendisi de sonradan aynı ağaçta hata verip satır kaybedebiliyordu. Artık sayfa korunup aynı seviyedeki kardeşleriyle C'deki gibi yeniden dengeleniyor; derinlik yalnızca kökte değişiyor. (2) **İndeks sayfasının hücre sayısına göre bölünmesi:** çok farklı boyutlu anahtarlarda bir yarı sayfaya sığmıyor ve `INSERT` `cells exceed usable B-tree page capacity` ile düşüyordu (C'de başarılı); bölünme noktası artık iki yarının da sığdığı en yakın konum (önceden çalışan bölünmeler bayt bayt aynı). (3) **`PRAGMA integrity_check`:** C'nin `btree.c` kontrolleri C'nin mesaj metinleriyle port edildi — B-tree düzeyinde 30 kontrolün 29'u (önce 6), SQL düzeyinde 24'ün 18'i (önce 2) C ile aynı. Ayrıntı: `saglik/integrity_check_parity.md`.

- **Testler**: `cargo test --workspace`: 542 geçti (9 yeni), bilinen 4 başarısız, 337 yok sayıldı. Üç tekrar betiği artık C `integrity_check` + `rtreecheck` geçiyor ve ağaç başına tek yaprak derinliği var; yeni Rust kontrolü düzeltme öncesi yazılmış bozuk dosyalarda C ile aynı mesajı veriyor. Hataya hiç girmeyen 13 senaryonun dosyaları önceki commit ile bayt bayt aynı. Farksal koşu: 12.960 + 1.920 çalıştırma, her dosya C `integrity_check` geçiyor. **Hız:** `W01`–`W13` komut sayıları değişmedi (hiçbir iş yükü düzeltilen yollara girmiyor; önceki kazançların hiçbiri bu hatalara dayanmıyordu). `W14` +%5: betik `PRAGMA integrity_check` ile bitiyor ve kontrol artık C'nin yaptığı işi yapıyor (eski 2,0 M komut, C 5,4 M, yeni 14,4 M). Doğrulanmayan: commit bazında ikiye bölme; `auto_vacuum` + iç sayfa yeniden dengeleme için ayrı C farksalı; kasıtlı bozulmuş 6.000 dosyanın 5'inde C hata verirken motor vermiyor (sayfa 1 hasarını şema yükleyici kabul ediyor).
- **Toplam**: `244.56 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.808x`** (disk WAL `0.781x`, bellek-içi `0.913x`)
- **Tepe RSS**: `17,952 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.51`** | `7.47` | `7.44` | **`1.009x`** | `0.968x` | `9.66` | **`0.78x`** | `8,820` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.43`** | `7.40` | `7.67` | **`0.969x`** | `0.796x` | `8.68` | **`0.86x`** | `9,724` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.96`** | `27.40` | `28.02` | **`0.998x`** | `0.757x` | `23.25` | **`1.20x`** | `9,724` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.83`** | `7.74` | `7.85` | **`0.997x`** | `0.546x` | `7.46` | **`1.05x`** | `9,788` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.10`** | `4.08` | `4.31` | **`0.951x`** | `0.238x` | `16.61` | **`0.25x`** | `9,788` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.65`** | `10.62` | `10.42` | **`1.022x`** | `0.981x` | `28.20` | **`0.38x`** | `9,980` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.34`** | `38.14` | `38.26` | **`1.002x`** | `0.984x` | `50.15` | **`0.76x`** | `12,460` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.40`** | `9.37` | `9.43` | **`0.997x`** | `0.534x` | `8.06` | **`1.17x`** | `12,460` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.95`** | `11.92` | `11.79` | **`1.014x`** | `0.986x` | `18.90` | **`0.63x`** | `12,460` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.12`** | `11.03` | `11.02` | **`1.009x`** | `0.362x` | `10.96` | **`1.01x`** | `12,708` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.18`** | `44.59` | `45.97` | **`0.983x`** | `0.935x` | `57.74` | **`0.78x`** | `17,832` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.43`** | `23.13` | `23.73` | **`0.987x`** | `0.583x` | `24.43` | **`0.96x`** | `17,888` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.68`** | `4.50` | `4.77` | **`0.981x`** | `0.649x` | `4.08` | **`1.15x`** | `17,920` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`34.98`** | `34.78` | `33.52` | **`1.043x`** | `0.937x` | `26.66` | **`1.31x`** | `17,888` | `fc7490e2b80c68e2` ✓ |

---

## 05d_duzeltme_wal_cokme — `performans-A@3707ece`

**Adım 5d — doğruluk düzeltmesi: WAL çökme atomikliği (performans amacı yok).** 5c envanterinde bulundu; WAL yazıcısı port edildiğinden beri vardı. Büyük bir yazma işleminde sayfa önbelleği taşıp sayfalar commit'ten önce `-wal`e yazıldığında ve taşmış bir sayfa yeniden değiştiğinde, `COMMIT` eski çerçeveyi yerinde yeniden yazar ve sonuna (commit çerçevesi dahil) yeni çerçeveler ekler. C bu sırada eklenen çerçeveleri tuz ve sağlama toplamı yerine 16 sıfır baytla yazar (`walEncodeFrame`, `iReCksum != 0`); geçerli zinciri ancak `walRewriteChecksums` kurar. Motor bu çerçeveleri gerçek tuz ve eski görüntüler üzerinden zincirlenmiş sağlama toplamıyla yazıyordu: çökme, eklenen çerçeveleri koruyup yerinde yeniden yazımı kaybederse **yarım uygulanmış bir işlem** görünür oluyordu (C aynı durumda işlemi atar). Artık C ile aynı: bekleyen yeniden yazım varken eklenen çerçeveler doğrulanamaz biçimde yazılıyor. Yazma sırası ve adım 1'deki çerçeve toplama değişmedi.

- **Testler**: `cargo test --workspace`: 546 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Dört yeni test, senkronlanmamış `-wal` yazımlarının her alt kümesini (test başına ~1.000–4.100 çökme görüntüsü) dener; kurtarma ya işlem öncesi durumu ya da işlemin tamamını vermeli. Düzeltme geri alınınca dördü de düşüyor. Süreç öldürme + blok geri yükleme betiği: düzeltmeden önce 600 satır ve iki güncellemeden biri uygulanmış; sonra C yazıcısıyla aynı sonuç (işlem atılıyor). **Hız:** toplam 0,999x; ölçüm iş yükleri önbellek taşırmadığı için `W13`/`W01` komut ve sistem çağrısı sayıları aynı. Doğrulanmayan: gerçek güç kesintisi (tüm çökme durumları sentetik). Kod okunarak bulunan ve düzeltilmeyen iki ara-durum farkı daha var (`journal_size_limit` kırpma hatası; `load_deserialized_image` içinde senkronsuz kapanış checkpoint'i).
- **Toplam**: `244.45 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.810x`** (disk WAL `0.784x`, bellek-içi `0.911x`)
- **Tepe RSS**: `17,864 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.49`** | `7.45` | `7.51` | **`0.997x`** | `0.965x` | `9.66` | **`0.78x`** | `8,944` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.52`** | `7.44` | `7.43` | **`1.012x`** | `0.806x` | `8.68` | **`0.87x`** | `9,784` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.90`** | `27.42` | `27.96` | **`0.998x`** | `0.756x` | `23.25` | **`1.20x`** | `9,784` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.87`** | `7.75` | `7.83` | **`1.005x`** | `0.549x` | `7.46` | **`1.06x`** | `9,848` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.12`** | `4.09` | `4.10` | **`1.004x`** | `0.239x` | `16.61` | **`0.25x`** | `9,848` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.81`** | `10.75` | `10.65` | **`1.015x`** | `0.996x` | `28.20` | **`0.38x`** | `10,040` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.30`** | `38.10` | `38.34` | **`0.999x`** | `0.983x` | `50.15` | **`0.76x`** | `12,584` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.38`** | `9.34` | `9.40` | **`0.997x`** | `0.533x` | `8.06` | **`1.16x`** | `12,584` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.90`** | `11.88` | `11.95` | **`0.996x`** | `0.982x` | `18.90` | **`0.63x`** | `12,584` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.14`** | `11.11` | `11.12` | **`1.002x`** | `0.362x` | `10.96` | **`1.02x`** | `12,832` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`44.90`** | `44.45` | `45.18` | **`0.994x`** | `0.929x` | `57.74` | **`0.78x`** | `17,784` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.39`** | `22.98` | `23.43` | **`0.998x`** | `0.582x` | `24.43` | **`0.96x`** | `17,840` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.72`** | `4.50` | `4.68` | **`1.010x`** | `0.655x` | `4.08` | **`1.16x`** | `17,872` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.02`** | `34.62` | `34.98` | **`1.001x`** | `0.938x` | `26.66` | **`1.31x`** | `17,840` | `fc7490e2b80c68e2` ✓ |

---

## 05e_duzeltme_guvence — `performans-A@e53008f`

**Adım 5e — doğruluk düzeltmeleri: güvence ve uyumluluk hataları (performans amacı yok).** (1) `PRAGMA secure_delete` yok sayılıyordu (satır dönmüyor, silinen hücrelerin içeriği dosyada kalıyordu); tüm biçimleriyle uygulandı, `ON` iken dosyada yalnızca canlı satırların baytları kalıyor. (2) C'de satır dönen ama burada dönmeyen pragmalar: `analysis_limit` (saklanıyor ve `ANALYZE` tarafından uygulanıyor), `data_version`, `cell_size_check` (yüklenen sayfalar C'deki gibi doğrulanıyor), `mmap_size`. (3) `ALTER TABLE ADD COLUMN` sütun metnini AST'den yeniden üretiyordu: `DEFAULT x'00'` → `x''` (varsayılan değer **kayboluyordu**), sabit olmayan varsayılan kabul ediliyordu; artık kullanıcının metni C'deki gibi aynen ekleniyor, C'nin kontrolleri C'nin sırası ve mesajlarıyla çalışıyor. (4) Tablosu olmayan veritabanının başlığı: şema biçimi ve kodlama ilk şema satırına kadar 0 kalıyor ve `PRAGMA encoding` o ana kadar geçerli. (5) İki WAL hata yolu: `journal_size_limit` kırpma hatası artık commit çerçevesi kalıcı olmuş bir `COMMIT`i başarısız yapmıyor; `load_deserialized_image` WAL'i senkronlu checkpoint ile kapatıyor.

- **Testler**: `cargo test --workspace`: 559 geçti (13 yeni), bilinen 4 başarısız, 337 yok sayıldı. `ADD COLUMN` farksalı 1.126 durumda C ile eşit: 1.118 (önce 407). Ayrışma bulucu (192 senaryo): hiçbir senaryo daha kötü katmanda bitmiyor, 11'i iyileşti. 13 düzen senaryosu önceki commit ile bayt bayt aynı. **Hız:** toplam 0,998x; komut sayıları aynı (`W08`/`W09` +%0,05). `W05` +%5 aynı komut sayısıyla (kod yerleşimi). **Bilerek C'den farklı bırakılan (mevcut entegrasyon testleri eski davranışı doğruluyor):** `ADD COLUMN` satır kontrolü hatası `SQLITE_CONSTRAINT` dönüyor (C: `SQLITE_ERROR`); `too many columns on t` (C: `… on sqlite_altertab_t`). Doğrulanmayan: deserialize yolu çalışan bir C ikilisine karşı.
- **Toplam**: `244.40 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.809x`** (disk WAL `0.783x`, bellek-içi `0.913x`)
- **Tepe RSS**: `17,888 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.45`** | `7.42` | `7.49` | **`0.995x`** | `0.961x` | `9.66` | **`0.77x`** | `8,868` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.57`** | `7.48` | `7.52` | **`1.005x`** | `0.810x` | `8.68` | **`0.87x`** | `9,708` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.43`** | `27.32` | `27.90` | **`0.983x`** | `0.743x` | `23.25` | **`1.18x`** | `9,708` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.66`** | `7.62` | `7.87` | **`0.973x`** | `0.534x` | `7.46` | **`1.03x`** | `9,772` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.30`** | `4.29` | `4.12` | **`1.046x`** | `0.250x` | `16.61` | **`0.26x`** | `9,772` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.69`** | `10.59` | `10.81` | **`0.989x`** | `0.985x` | `28.20` | **`0.38x`** | `9,964` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.78`** | `38.56` | `38.30` | **`1.013x`** | `0.995x` | `50.15` | **`0.77x`** | `12,508` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.40`** | `9.37` | `9.38` | **`1.002x`** | `0.534x` | `8.06` | **`1.17x`** | `12,508` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.83`** | `11.78` | `11.90` | **`0.994x`** | `0.976x` | `18.90` | **`0.63x`** | `12,508` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.11`** | `11.04` | `11.14` | **`0.998x`** | `0.362x` | `10.96` | **`1.01x`** | `12,756` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`44.84`** | `44.35` | `44.90` | **`0.999x`** | `0.928x` | `57.74` | **`0.78x`** | `17,716` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.63`** | `23.32` | `23.39` | **`1.010x`** | `0.588x` | `24.43` | **`0.97x`** | `17,764` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.62`** | `4.51` | `4.72` | **`0.979x`** | `0.641x` | `4.08` | **`1.13x`** | `17,796` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.09`** | `34.82` | `35.02` | **`1.002x`** | `0.940x` | `26.66` | **`1.32x`** | `17,764` | `fc7490e2b80c68e2` ✓ |

---

## 05f_duzeltme_fts5 — `performans-A@e4cdaff`

**Adım 5f — doğruluk düzeltmesi: FTS5'in C ile iki yönlü uyumu (performans amacı yok).** Bu değişiklikten önce iki motor bir FTS5 tablosunu güvenle paylaşamıyordu (farksal koşuda 300 tohumun 300'ü en az bir yönde başarısızdı). Düzeltilenler: **silme bayrağı** (okuma: C'nin `UPDATE`inden sonra `MATCH` satır kaçırıyor, ardından `'optimize'` o kayıtları kalıcı siliyordu; yazma: aynı boşaltmada silinip yeniden eklenen satırın bayrağı kayboluyordu); **64 KiB'ı aşan yaprak** (C: bozuk); **`contentless_delete=1`** kayıt biçimi; **ayraçlar** (unicode61, ascii, porter, trigram C'den bayt düzeyinde port edildi — katlama/aksan/kök bulma farklıydı, C `checksum mismatch` veriyordu); **çakışma yan tümceleri** (`INSERT OR REPLACE` vb.); **`contentless_unindexed=1`, `locale=1`**; FTS5 içinde gerçel→metin çevrimi. Ayrıca sorgu dilbilgisi, `bm25`/`rank`, `highlight`, `snippet`, `fts5vocab`, özel komutlar ve hata metinleri C'ye getirildi. **Kapsam dışı (yapısal, ayrı karar):** segment birleştirme takvimi ve boşaltma zamanı, yaprak boyutu hedefi ve doclist indeksi, yapı çerezi.

- **Testler**: `cargo test --workspace`: 597 geçti (38 yeni; 34'ü C CLI'sinden üretilmiş, gömülü C gölge tablo görüntüleriyle), bilinen 4 başarısız, 337 yok sayıldı. Yeni 34 durumun 32'si önceki kaynakta düşüyor. Farksal koşu (rastgele yapılandırma, işlemler ve sorgular; Rust yazar / C yazar / sırayla yazar; dört motor-dosya birleşiminde bütünlük kontrolü): 3.600 tohum / 10.800 çalıştırma, 0 hata (önce 900 çalıştırmanın 863'ü). 60 tohum yalnızca `secure-delete=1` ile farklı; her biri C'nin kendi indeksini bozmasına dayanıyor (51'i yalnız C'nin yazdığı geçmişte). **Hız:** toplam 1,004x; `W11` +%2,6 (komut sayısı 248,20 M → 247,86 M; `bm25`in C ile birebir aynı olması için eşleşen satır başına daha çok bellek). Düzeltilmeyen: `fts5_locale()` yok; `LIMIT` ötesindeki satırlar için yardımcı fonksiyon değerlendirmesi; `%_config` sürümü bağlanırken denetlenmiyor.
- **Toplam**: `246.46 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.816x`** (disk WAL `0.790x`, bellek-içi `0.922x`)
- **Tepe RSS**: `18,104 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.49`** | `7.46` | `7.45` | **`1.005x`** | `0.965x` | `9.66` | **`0.78x`** | `8,992` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.58`** | `7.54` | `7.57` | **`1.002x`** | `0.812x` | `8.68` | **`0.87x`** | `9,828` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.60`** | `27.45` | `27.43` | **`1.006x`** | `0.748x` | `23.25` | **`1.19x`** | `9,828` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.80`** | `7.65` | `7.66` | **`1.018x`** | `0.544x` | `7.46` | **`1.05x`** | `9,892` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.41`** | `4.37` | `4.30` | **`1.024x`** | `0.256x` | `16.61` | **`0.27x`** | `9,892` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.50`** | `10.40` | `10.69` | **`0.982x`** | `0.967x` | `28.20` | **`0.37x`** | `10,024` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.55`** | `38.39` | `38.78` | **`0.994x`** | `0.989x` | `50.15` | **`0.77x`** | `12,504` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.41`** | `9.37` | `9.40` | **`1.002x`** | `0.535x` | `8.06` | **`1.17x`** | `12,504` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.97`** | `11.69` | `11.83` | **`1.012x`** | `0.987x` | `18.90` | **`0.63x`** | `12,504` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.14`** | `11.06` | `11.11` | **`1.003x`** | `0.363x` | `10.96` | **`1.02x`** | `12,688` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.94`** | `45.31` | `44.84` | **`1.025x`** | `0.950x` | `57.74` | **`0.80x`** | `17,732` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.65`** | `23.47` | `23.63` | **`1.001x`** | `0.588x` | `24.43` | **`0.97x`** | `17,804` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.85`** | `4.57` | `4.62` | **`1.048x`** | `0.672x` | `4.08` | **`1.19x`** | `17,836` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.56`** | `35.18` | `35.09` | **`1.013x`** | `0.952x` | `26.66` | **`1.33x`** | `17,804` | `fc7490e2b80c68e2` ✓ |

---

## 05g_hizalama_dosya — `performans-A@fc4f0ca`

**Adım 5g — C'ye hizalama: dosya düzeyi, bedelsiz sınıflar.** Aynı ifade dizisi için yazılan dosyayı C'nin yazdığına yaklaştırır. (1) **Değişim sayacı** 0'dan başlıyor ve WAL commit'inde C gibi sabit kalıyor. (2) **Şema çerezi** satır başına değil ifade başına artıyor; `PRAGMA schema_version` her ifadeden sonra C ile aynı. (3) **Satır sıraları**: sanal tablo ve gölge tablolarının `sqlite_schema` sırası, `ANALYZE`in `sqlite_stat1` sırası, `CREATE TABLE … UNIQUE` yer tutucu satır dizisi, `VACUUM` ve `DROP TABLE` sırası. (4) **`REAL` sütunda tam değerli sayı** C'nin kuralıyla tamsayı olarak saklanıyor (dosya 1,8 kat küçülüyor); okuma tarafı da düzeltildi (C'nin yazdığı dosyada bu değerler bazı yollarda `integer` dönüyordu). `jsonb_set` vb. metni C'nin eleman türüyle yazıyor. (5) **Serbest bırakılan sayfalar** artık günlüğe alınmıyor, sıfırlanmıyor ve yeniden yazılmıyor (`DROP TABLE` 6.260 → 13 yazma çağrısı; C 25); `secure_delete` `ON`/`FAST`/`OFF` sayfa kararları C ile aynı. (6) **Serbest liste sırası** (ilk gövdenin ilk yaprağı / en yakın yaprak) ve **satır başına yazma sırası** (önce indeksler, sonra tablo satırı; `UPDATE`te yalnızca C'nin yeniden yazdığı indeksler). (7) Günlük başlığı 512 bayt; kısmi checkpoint C'nin sayfa seçimi; yeni dosyalar 0644 ve yan dosyalar veritabanının iznini alıyor. İki ek bulgu düzeltildi: iki `UNIQUE` ihlalinde hata sırası C'ninki oldu.

- **Testler**: `cargo test --workspace`: 626 geçti (29 yeni; 19'u C'nin yazdığı dosyayla bayt eşitliği), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu (192 senaryo):** hiçbir senaryo daha kötü katmanda bitmiyor; **22'si artık C ile bayt bayt aynı (önce 0)**, 42'si yalnızca kullanılmayan baytlarda farklı. **Karar eşliği** (izli C derlemesi ile Rust karar kayıtlarının olay olay karşılaştırması, 234 betik): çerez 234/234, `REAL` saklama 234/234, JSONB 234/234, şema satırları 227/234, serbest bırakma 181/234, tahsis 191/234, yazma sırası 170/234 — son üçü ağaç şekline bağlı; uyuşmayan betikler kapsam dışı sınıfların (silmede yeniden dengeleme, indeks doluluğu, bölünmeler, FTS5/R-Tree yazıcıları) ifadelerinde ayrışıyor. Ayrıntı: `saglik/karar_esligi_5g.md`. **Çökme güvenliği:** 258 geri alma betiği ve commit sırasında öldürülen 2.592 kurtarma (iki yazıcı × iki kurtarıcı), 0 hata (tmpfs; gerçek güç kesintisi yok). **Hız:** toplam 0,998x; en büyük komut artışı `W10` +%0,96. Hizalanmayan/doğrulanmayan: serbest listeden alınan sayfa hâlâ okunup sıfırlanıyor; indeks taramasıyla `DELETE` ve var olan satır kimliğinde `REPLACE` yazma sırası; `FAST` modunda dosyadaki artık C'den biraz fazla (nedeni incelenmedi); `PRAGMA optimize` yok.
- **Toplam**: `245.92 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.814x`** (disk WAL `0.788x`, bellek-içi `0.918x`)
- **Tepe RSS**: `18,144 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.59`** | `7.55` | `7.49` | **`1.014x`** | `0.979x` | `9.66` | **`0.79x`** | `9,144` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.44` | `7.58` | **`0.991x`** | `0.804x` | `8.68` | **`0.86x`** | `10,044` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.74`** | `27.45` | `27.60` | **`1.005x`** | `0.751x` | `23.25` | **`1.19x`** | `10,044` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.82`** | `7.72` | `7.80` | **`1.002x`** | `0.545x` | `7.46` | **`1.05x`** | `10,108` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.37`** | `4.36` | `4.41` | **`0.992x`** | `0.254x` | `16.61` | **`0.26x`** | `10,108` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.52`** | `10.44` | `10.50` | **`1.002x`** | `0.970x` | `28.20` | **`0.37x`** | `10,176` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.76`** | `38.56` | `38.55` | **`1.005x`** | `0.995x` | `50.15` | **`0.77x`** | `12,500` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.45`** | `9.42` | `9.41` | **`1.004x`** | `0.537x` | `8.06` | **`1.17x`** | `12,500` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.88`** | `11.86` | `11.97` | **`0.993x`** | `0.980x` | `18.90` | **`0.63x`** | `12,500` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.13`** | `11.09` | `11.14` | **`0.999x`** | `0.362x` | `10.96` | **`1.02x`** | `12,684` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.95`** | `45.67` | `45.94` | **`1.000x`** | `0.951x` | `57.74` | **`0.80x`** | `17,976` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.39`** | `23.11` | `23.65` | **`0.989x`** | `0.582x` | `24.43` | **`0.96x`** | `18,000` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.73`** | `4.42` | `4.85` | **`0.977x`** | `0.656x` | `4.08` | **`1.16x`** | `18,032` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.06`** | `34.59` | `35.56` | **`0.986x`** | `0.939x` | `26.66` | **`1.31x`** | `18,000` | `fc7490e2b80c68e2` ✓ |

---

## 05h_hizalama_sql — `performans-A@a0cffa4`

**Adım 5h — C'ye hizalama: SQL düzeyi, bedelsiz sınıflar.** SQL üzerinden gözlenen sonuçları C ile eşitler. (A) **Gerçel ↔ metin**: C'nin `sqlite3FpDecode` (17 anlamlı basamak, gidiş-dönüş indirgemesi) ve `sqlite3AtoF` davranışı tüm motorda (`CAST`, `||`, TEXT yakınlığı, `quote`, JSON, CLI); `printf` C'nin `sqlite3_str_vappendf` portu. (B) **Fonksiyonlar**: `substr` (negatif başlangıç, BLOB BLOB kalır), `hex`/`octet_length` (saklanan kodlama), `replace`, `instr`, `trim`, `lower`/`upper`, `round`, `ln`/`log`, `quote`, `like`/`glob`; listede olup uygulanmamış `concat`, `concat_ws`, `unistr`, `unistr_quote` eklendi; `count(DISTINCT)` 1 ile 1.0'ı tek değer sayıyor; `generate_series` metin işlenenli kısıt. (C) **İfade anlambilimi**: R-Tree'ye `INSERT OR FAIL`; satır kimliği 2^63−1'de C'nin rastgele aday algoritması; `sqlite_schema`dan CTAS; `PRAGMA table_info` varsayılan metni; üretilmiş sütun kontrolleri. (D) **`ORDER BY`sız `GROUP BY`** artık C gibi anahtar sırasıyla dönüyor (ölçülen bedel: komut sayısı 1,0000–1,0002). (E) `integrity_check`in üç SQL düzeyi kontrolü daha. (F) **CLI**: `Parse error near line N` + işaretçi satırları, `Error near line N`, hata öncesi satırlar, çıkış kodları, çıktı modlarının biçimi. Onaylanan iki test güncellemesi: `ADD COLUMN` satır kontrolü hatası `SQLITE_ERROR`; `too many columns on sqlite_altertab_t`.

- **Testler**: `cargo test --workspace`: 655 geçti (29 yeni), bilinen 4 başarısız, 337 yok sayıldı. Farksal: 109.942 gerçel değer × 16 SQL yolu + 8 CLI modu (1.019.281 çıktı satırı) C ile fark 0 (önce 754.128); fonksiyon taraması 257.527 çağrıda fark 3.317 (önce 62.648) — kalanların 2.796'sı tarih/saat ailesi (`date.c` port edilmedi, `timediff` yok), 504'ü JSON ayrıntıları. Ayrışma bulucu (234 senaryo): hiçbiri kötüleşmedi; stdout C ile eşit 230 → 233, stderr 228 → 234. **Hız:** toplam 1,005x; komut sayıları aynı, yalnız `W14` +%1,4 (`integrity_check` daha çok kontrol yapıyor). `W06` +%5,6 süre, aynı komut sayısıyla (kod yerleşimi; komut önbelleği ıskası +%14). **Hizalanmayan:** sanal tablolarda `total_changes()`/`changes()` (C gölge tablo yazımlarını sayıyor); tarih/saat fonksiyonları; UTF-16 veritabanında BINARY metin sırası; `PRAGMA optimize`; `integrity_check`te kesin olmayan kayan nokta mesajı ve satırlardan sonra hata; `fts5_locale()`; CLI'de savunmacı kip ve hazırlama zamanı hatasının `Parse error` olarak sınıflanmadığı durumlar (ör. üretilmiş sütun hatası). CLI crate'inin kendi üç testi C'den farklı davranışı doğruladığı için (`-json` tek satır, CSV satır sonu/tırnaklama, çok satırlı dizgede CRLF) o davranışlar bırakıldı.
- **Toplam**: `247.92 ms` (taban `328.95 ms`, C `294.85 ms`) — geomean `Rust/C` **`0.819x`** (disk WAL `0.791x`, bellek-içi `0.930x`)
- **Tepe RSS**: `18,244 KiB` (C: `6,160 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.64`** | `7.59` | `7.59` | **`1.006x`** | `0.984x` | `9.66` | **`0.79x`** | `9,084` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.64`** | `7.56` | `7.51` | **`1.017x`** | `0.818x` | `8.68` | **`0.88x`** | `9,924` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.79`** | `27.52` | `27.74` | **`1.002x`** | `0.753x` | `23.25` | **`1.20x`** | `9,924` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.72`** | `7.61` | `7.82` | **`0.987x`** | `0.538x` | `7.46` | **`1.03x`** | `9,988` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.09`** | `4.06` | `4.37` | **`0.936x`** | `0.237x` | `16.61` | **`0.25x`** | `9,988` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`11.07`** | `11.04` | `10.52` | **`1.052x`** | `1.020x` | `28.20` | **`0.39x`** | `10,116` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.38`** | `38.19` | `38.76` | **`0.990x`** | `0.985x` | `50.15` | **`0.77x`** | `12,604` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.44`** | `9.40` | `9.45` | **`0.998x`** | `0.536x` | `8.06` | **`1.17x`** | `12,732` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.02`** | `11.97` | `11.88` | **`1.012x`** | `0.992x` | `18.90` | **`0.64x`** | `12,732` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.11`** | `11.08` | `11.13` | **`0.997x`** | `0.362x` | `10.96` | **`1.01x`** | `12,916` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.64`** | `45.18` | `45.95` | **`0.993x`** | `0.944x` | `57.74` | **`0.79x`** | `18,184` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.59`** | `23.62` | `23.39` | **`1.051x`** | `0.612x` | `24.43` | **`1.01x`** | `18,244` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.85`** | `4.73` | `4.73` | **`1.024x`** | `0.672x` | `4.08` | **`1.19x`** | `18,276` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.94`** | `35.64` | `35.06` | **`1.025x`** | `0.963x` | `26.66` | **`1.35x`** | `18,244` | `fc7490e2b80c68e2` ✓ |
