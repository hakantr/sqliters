# `performans-A` — Saf Rust Optimizasyon Adımları Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-13T02:47:52+03:00`
- **Yöntem**: `speedbench` `W01`–`W14`; 1 ısınma + 9 sıralı (interleaved) deneme, medyan; süreçler `taskset -c 20` ile tek çekirdeğe sabit; veritabanı dosyaları `tmpfs (/run/user/1000/sqliters-perf)` üzerinde (fsync gürültüsü yok). Tüm aşamaların saklanmış ikilileri ve C referansı **aynı oturumda** ölçüldü.
- **Kısıtlar (tüm aşamalar)**: `#![forbid(unsafe_code)]`, sıfır dış bağımlılık, SWAR / SIMD / `asm!` yok, SQLite Format-3 ve WAL protokolü uyumu, C ile birebir `FNV-1a` çıktı özeti.

> **Dikkat — farklı donanım.** Üst dizindeki `00`–`03` anlık görüntüleri ve `benchmark_report.md` başka bir makinede (`AMD Ryzen 7 8845HS`) ölçülmüştür; bu dizindeki sayılar o tabloyla doğrudan karşılaştırılamaz. Buradaki `00_performans_taban` satırı, aynı `performans@4ccdc64` kodunun **bu makinedeki** ölçümüdür ve `performans-A` adımlarının bazıdır.
>
> **Ölçülmeyen.** `./sqltest` uçtan uca CLI komut seti (126 örnek) ve C `rss_probe` tabanlı çocuk süreç RSS ölçümü bu aşamalarda koşulmadı; JSON'larda `sqltest_cli_command_set` alanı `null`.

**Dosyalar**: her aşama için `NN_<aşama>.json` (deneme bazında süreler, RSS, özetler); `asamalar.json` (aşama tanımları, commit'ler, test sonuçları); `olc.py` (bu raporu üreten betik); `ek_resmi_run_speedbench_taban.json` (bazın `tools/run_speedbench.py` ile sabitlenmemiş, btrfs üzerindeki ilk resmi koşusu: Rust `723,80 ms` / C `647,26 ms`, geomean `1,12x` — fsync ve zamanlama gürültüsü içerir).

## 1. Aşama Özeti

| Aşama | Git (`dal@commit`) | İkili (`KiB`) | `.text` (`KiB`) | Toplam (`ms`) | Geomean (`vs taban`) | Geomean (`vs C`) | Toplam (`vs C`) | Tepe RSS (`KiB`) | Özetler |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`295.04`** | `—` | **`1.000x`** | **`1.000x`** | `6,176` | `—` |
| **00_performans_taban** | `performans@4ccdc64` | `6,382.7` | `5,412.9` | **`328.62`** | `1.000x` | **`1.184x`** | `1.114x` | `18,080` | 14/14 ✓ |
| **01_a1_wal_islem** | `performans-A@693ac8b` | `6,388.4` | `5,414.5` | **`314.44`** | `0.936x` | **`1.109x`** | `1.066x` | `18,064` | 14/14 ✓ |
| **02_a2_jsonb_indeks** | `performans-A@e2f7843` | `6,417.4` | `5,438.1` | **`289.18`** | `0.860x` | **`1.018x`** | `0.980x` | `17,732` | 14/14 ✓ |
| **03_a3_nokta_dml** | `performans-A@946f61e` | `6,456.6` | `5,472.7` | **`281.43`** | `0.819x` | **`0.970x`** | `0.954x` | `17,672` | 14/14 ✓ |
| **04_a4_kapsayan_indeks** | `performans-A@40e275d` | `6,644.6` | `5,647.8` | **`262.85`** | `0.712x` | **`0.844x`** | `0.891x` | `18,264` | 14/14 ✓ |
| **05_a5_rtree** | `performans-A@700ded4` | `6,650.1` | `5,652.7` | **`242.95`** | `0.682x` | **`0.808x`** | `0.823x` | `17,912` | 14/14 ✓ |
| **05b_duzeltme_dengesiz_agac** | `performans-A@1c12e74` | `6,717.4` | `5,713.6` | **`244.11`** | `0.682x` | **`0.807x`** | `0.827x` | `17,868` | 14/14 ✓ |
| **05d_duzeltme_wal_cokme** | `performans-A@3707ece` | `6,717.5` | `5,713.7` | **`245.45`** | `0.684x` | **`0.810x`** | `0.832x` | `17,928` | 14/14 ✓ |
| **05e_duzeltme_guvence** | `performans-A@e53008f` | `6,734.0` | `5,727.9` | **`246.08`** | `0.686x` | **`0.812x`** | `0.834x` | `17,836` | 14/14 ✓ |
| **05f_duzeltme_fts5** | `performans-A@e4cdaff` | `6,751.0` | `5,734.4` | **`246.19`** | `0.688x` | **`0.814x`** | `0.834x` | `18,156` | 14/14 ✓ |
| **05g_hizalama_dosya** | `performans-A@fc4f0ca` | `6,802.1` | `5,778.2` | **`247.29`** | `0.689x` | **`0.816x`** | `0.838x` | `18,112` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_taban | 01_a1_wal_islem | 02_a2_jsonb_indeks | 03_a3_nokta_dml | 04_a4_kapsayan_indeks | 05_a5_rtree | 05b_duzeltme_dengesiz_agac | 05d_duzeltme_wal_cokme | 05e_duzeltme_guvence | 05f_duzeltme_fts5 | 05g_hizalama_dosya |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.65` | `7.70` (**0.80x**) | `7.38` (**0.77x**) | `7.46` (**0.77x**) | `7.48` (**0.78x**) | `7.32` (**0.76x**) | `7.44` (**0.77x**) | `7.51` (**0.78x**) | `7.47` (**0.77x**) | `7.40` (**0.77x**) | `7.42` (**0.77x**) | `7.56` (**0.78x**) |
| `W02_create_indexes` | `8.66` | `9.32` (**1.07x**) | `8.99` (**1.04x**) | `8.50` (**0.98x**) | `7.51` (**0.87x**) | `7.51` (**0.87x**) | `7.59` (**0.88x**) | `7.50` (**0.86x**) | `7.54` (**0.87x**) | `7.53` (**0.87x**) | `7.60` (**0.88x**) | `7.53` (**0.87x**) |
| `W03_pk_point_select` | `23.62` | `36.62` (**1.55x**) | `28.31` (**1.20x**) | `28.09` (**1.19x**) | `27.66` (**1.17x**) | `27.47` (**1.16x**) | `28.00` (**1.19x**) | `27.82` (**1.18x**) | `27.73` (**1.17x**) | `27.51` (**1.17x**) | `27.57` (**1.17x**) | `27.97` (**1.18x**) |
| `W04_secondary_idx_range` | `7.51` | `14.48` (**1.93x**) | `12.57` (**1.67x**) | `12.63` (**1.68x**) | `12.69` (**1.69x**) | `7.73` (**1.03x**) | `7.95` (**1.06x**) | `7.81` (**1.04x**) | `7.87` (**1.05x**) | `7.76` (**1.03x**) | `7.76` (**1.03x**) | `7.72` (**1.03x**) |
| `W05_full_scan_filter_agg` | `16.56` | `17.27` (**1.04x**) | `17.89` (**1.08x**) | `17.52` (**1.06x**) | `18.02` (**1.09x**) | `4.40` (**0.27x**) | `4.31` (**0.26x**) | `4.09` (**0.25x**) | `4.10` (**0.25x**) | `4.31` (**0.26x**) | `4.40` (**0.27x**) | `4.38` (**0.26x**) |
| `W06_join_group_order` | `28.22` | `10.90` (**0.39x**) | `10.72` (**0.38x**) | `10.53` (**0.37x**) | `10.64` (**0.38x**) | `10.60` (**0.38x**) | `10.43` (**0.37x**) | `10.66` (**0.38x**) | `10.80` (**0.38x**) | `10.63` (**0.38x**) | `10.44` (**0.37x**) | `10.48` (**0.37x**) |
| `W07_window_functions` | `50.27` | `38.83` (**0.77x**) | `38.59` (**0.77x**) | `38.66` (**0.77x**) | `38.76` (**0.77x**) | `38.49` (**0.77x**) | `38.41` (**0.76x**) | `38.30` (**0.76x**) | `38.45` (**0.77x**) | `38.79` (**0.77x**) | `38.53` (**0.77x**) | `38.92` (**0.77x**) |
| `W08_indexed_update_delete` | `8.00` | `17.51` (**2.19x**) | `16.55` (**2.07x**) | `15.81` (**1.98x**) | `9.49` (**1.19x**) | `9.53` (**1.19x**) | `9.39` (**1.17x**) | `9.38` (**1.17x**) | `9.35` (**1.17x**) | `9.41` (**1.18x**) | `9.43` (**1.18x**) | `9.47` (**1.18x**) |
| `W09_like_text_scan` | `18.93` | `11.96` (**0.63x**) | `12.02` (**0.64x**) | `12.03` (**0.64x**) | `11.69` (**0.62x**) | `11.77` (**0.62x**) | `11.79` (**0.62x**) | `11.98` (**0.63x**) | `11.95` (**0.63x**) | `11.83` (**0.62x**) | `11.97` (**0.63x**) | `11.88` (**0.63x**) |
| `W10_jsonb_extract_agg` | `11.04` | `30.63` (**2.77x**) | `30.81` (**2.79x**) | `11.65` (**1.05x**) | `11.18` (**1.01x**) | `10.99` (**1.00x**) | `11.01` (**1.00x**) | `11.06` (**1.00x**) | `11.07` (**1.00x**) | `11.09` (**1.00x**) | `11.08` (**1.00x**) | `11.16` (**1.01x**) |
| `W11_fts5_index_and_bm25` | `57.46` | `47.65` (**0.83x**) | `48.20` (**0.84x**) | `47.25` (**0.82x**) | `46.88` (**0.82x**) | `49.01` (**0.85x**) | `45.06` (**0.78x**) | `44.94` (**0.78x**) | `45.39` (**0.79x**) | `45.63` (**0.79x**) | `45.91` (**0.80x**) | `46.63` (**0.81x**) |
| `W12_rtree_insert_and_bbox` | `24.42` | `41.26` (**1.69x**) | `40.93` (**1.68x**) | `40.36` (**1.65x**) | `40.81` (**1.67x**) | `40.00` (**1.64x**) | `23.45` (**0.96x**) | `23.63` (**0.97x**) | `24.10` (**0.99x**) | `24.12` (**0.99x**) | `23.76` (**0.97x**) | `23.60` (**0.97x**) |
| `W13_autocommit_wal_txns` | `4.06` | `7.20` (**1.77x**) | `4.84` (**1.19x**) | `4.97` (**1.22x**) | `4.91` (**1.21x**) | `4.85` (**1.19x**) | `4.78` (**1.18x**) | `4.68` (**1.15x**) | `4.72` (**1.16x**) | `4.73` (**1.16x**) | `4.79` (**1.18x**) | `4.81` (**1.18x**) |
| `W14_cli_sql_script` | `26.63` | `37.29` (**1.40x**) | `36.65` (**1.38x**) | `33.73` (**1.27x**) | `33.73` (**1.27x**) | `33.17` (**1.25x**) | `33.36` (**1.25x**) | `34.74` (**1.30x**) | `34.89` (**1.31x**) | `35.35` (**1.33x**) | `35.52` (**1.33x**) | `35.17` (**1.32x**) |
| **Toplam** | **`295.04`** | **`328.62`** (**1.11x**) | **`314.44`** (**1.07x**) | **`289.18`** (**0.98x**) | **`281.43`** (**0.95x**) | **`262.85`** (**0.89x**) | **`242.95`** (**0.82x**) | **`244.11`** (**0.83x**) | **`245.45`** (**0.83x**) | **`246.08`** (**0.83x**) | **`246.19`** (**0.83x**) | **`247.29`** (**0.84x**) |

---

## 00_performans_taban — `performans@4ccdc64`

**Baz.** `performans` dalının (Saf Rust tavanı, P1-C) bu makinedeki ölçümü; `performans-A` adımlarının karşılaştırma noktası.

- **Testler**: `cargo test --workspace`: 517 geçti, 4 başarısız, 337 yok sayıldı. Başarısız 4 test (`tests/phase7_vtab_framework_and_pragma_tables.rs` içindeki `*_differential_parity`) referans ikili için başka bir makinenin yolunu sabit kodluyor; bu makinede tüm aşamalarda aynı şekilde düşüyor. (C uyumluluk testleri için `sqlite3` 3.54.1 referans ikilisi `PATH`e eklenerek koşuldu.)
- **Toplam**: `328.62 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`1.184x`** (disk WAL `1.096x`, bellek-içi `1.572x`)
- **Tepe RSS**: `18,080 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.70`** | `7.66` | `1.000x` | `9.65` | **`0.80x`** | `8,684` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.32`** | `9.28` | `1.000x` | `8.66` | **`1.07x`** | `9,592` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`36.62`** | `36.37` | `1.000x` | `23.62` | **`1.55x`** | `9,592` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`14.48`** | `14.26` | `1.000x` | `7.51` | **`1.93x`** | `9,720` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.27`** | `17.15` | `1.000x` | `16.56` | **`1.04x`** | `9,720` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.90`** | `10.86` | `1.000x` | `28.22` | **`0.39x`** | `9,720` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.83`** | `38.58` | `1.000x` | `50.27` | **`0.77x`** | `12,168` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`17.51`** | `17.30` | `1.000x` | `8.00` | **`2.19x`** | `12,168` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.96`** | `11.93` | `1.000x` | `18.93` | **`0.63x`** | `12,168` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.63`** | `30.52` | `1.000x` | `11.04` | **`2.77x`** | `13,204` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.65`** | `46.34` | `1.000x` | `57.46` | **`0.83x`** | `17,776` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`41.26`** | `39.89` | `1.000x` | `24.42` | **`1.69x`** | `17,880` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`7.20`** | `7.07` | `1.000x` | `4.06` | **`1.77x`** | `17,912` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`37.29`** | `36.90` | `1.000x` | `26.63` | **`1.40x`** | `15,760` | `fc7490e2b80c68e2` ✓ |

---

## 01_a1_wal_islem — `performans-A@693ac8b`

**Adım 1 — WAL okuma/yazma işlemi açılış-kapanış maliyeti.** Sistem çağrısı sayısı zaten C ile aynıydı; fark kullanıcı alanındaydı (okuma işlemi başına ~32 mutex turu, tek tek doğrulanan ~70 shm kelime okuması). `validate_offset` satır-içi tek koşul oldu (ayrıntılı hata yolu soğuk fonksiyonda aynen duruyor); bağlantı başına bölge-0 önbelleği, kilitsiz kapalı/zehirli bayrağı, birleşik kilit-sonrası doğrulama (aynı kontroller, aynı sıra); bayt sırasına göre kelime döngülü WAL sağlama toplamı; ardışık WAL çerçeveleri tek konumlu yazmada (≤ 64 KiB) toplanıyor. Hedef: `W03`, `W13`, `W04`.

- **Testler**: `cargo test --workspace`: 517 geçti, bilinen 4 başarısız, 337 yok sayıldı. Windows arka ucu derlenmedi (koda dokunulmadı; bölge-0 önbelleğinin dayandığı değişmez orada yalnızca kod okunarak kontrol edildi).
- **Toplam**: `314.44 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`1.109x`** (disk WAL `1.007x`, bellek-içi `1.577x`)
- **Tepe RSS**: `18,064 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.38`** | `7.34` | `7.70` | **`0.959x`** | `0.959x` | `9.65` | **`0.77x`** | `8,544` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`8.99`** | `8.95` | `9.32` | **`0.964x`** | `0.964x` | `8.66` | **`1.04x`** | `9,376` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.31`** | `27.72` | `36.62` | **`0.773x`** | `0.773x` | `23.62` | **`1.20x`** | `9,376` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.57`** | `12.42` | `14.48` | **`0.868x`** | `0.868x` | `7.51` | **`1.67x`** | `9,440` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.89`** | `17.74` | `17.27` | **`1.036x`** | `1.036x` | `16.56` | **`1.08x`** | `9,440` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.72`** | `10.69` | `10.90` | **`0.983x`** | `0.983x` | `28.22` | **`0.38x`** | `9,504` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.59`** | `38.43` | `38.83` | **`0.994x`** | `0.994x` | `50.27` | **`0.77x`** | `11,928` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`16.55`** | `16.26` | `17.51` | **`0.945x`** | `0.945x` | `8.00` | **`2.07x`** | `11,928` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.02`** | `11.72` | `11.96` | **`1.004x`** | `1.004x` | `18.93` | **`0.64x`** | `11,928` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.81`** | `30.66` | `30.63` | **`1.006x`** | `1.006x` | `11.04` | **`2.79x`** | `12,964` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`48.20`** | `47.72` | `47.65` | **`1.011x`** | `1.011x` | `57.46` | **`0.84x`** | `17,736` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.93`** | `39.72` | `41.26` | **`0.992x`** | `0.992x` | `24.42` | **`1.68x`** | `17,780` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.84`** | `4.56` | `7.20` | **`0.672x`** | `0.672x` | `4.06` | **`1.19x`** | `17,812` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.65`** | `36.25` | `37.29` | **`0.983x`** | `0.983x` | `26.63` | **`1.38x`** | `17,780` | `fc7490e2b80c68e2` ✓ |

---

## 02_a2_jsonb_indeks — `performans-A@e2f7843`

**Adım 2 — ham indeks yaprağı yeniden dengeleme ve indeks sıralı akışlı kümeleme.** `W10`daki fark JSON'dan değil iki genel yoldan geliyordu: (1) dolu indeks yaprağına her eklemede üst düğüm + 3 kardeş yaprak hücre hücre `Vec`e ayrıştırılıp yeniden yazılıyordu; `try_insert_index_balance_raw` aynı dağıtımı ham sayfa baytları üzerinde yapıyor (sayfa çıktısı eski yolla bayt bayt aynı, desteklemediği her durumda sayfaya dokunmadan eski yola düşüyor). (2) Akışlı kümeleme planları kapsayıcı olmayan indeks taramasını reddediyordu; yeni `IndexOrderedTableLookup` erişim yolu eklendi. Ayrıca kayıt karşılaştırmada tamsayı-tamsayı hızlı yolu. **Davranış düzeltmesi:** `HashGroupByPlan` tamsayı `1` ile gerçel `1.0` anahtarlarını iki ayrı grup sayıyordu; artık SQLite'taki gibi tek grup. Hedef: `W10`; yan kazanç `W02`, `W08`, `W14`.

- **Testler**: `cargo test --workspace`: 520 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. 9.000 satırlık rastgele indeksli yükte yazılan veritabanı baz ikilininkiyle bayt bayt aynı ve C `integrity_check` geçiyor. Sayfa başına ayrılmış bayt (reserved bytes) kullanan veritabanlarında denenmedi.
- **Toplam**: `289.18 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`1.018x`** (disk WAL `0.990x`, bellek-içi `1.127x`)
- **Tepe RSS**: `17,732 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.46`** | `7.36` | `7.38` | **`1.010x`** | `0.969x` | `9.65` | **`0.77x`** | `8,680` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`8.50`** | `8.40` | `8.99` | **`0.946x`** | `0.912x` | `8.66` | **`0.98x`** | `9,512` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.09`** | `27.52` | `28.31` | **`0.992x`** | `0.767x` | `23.62` | **`1.19x`** | `9,512` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.63`** | `12.47` | `12.57` | **`1.004x`** | `0.872x` | `7.51` | **`1.68x`** | `9,640` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.52`** | `17.46` | `17.89` | **`0.979x`** | `1.014x` | `16.56` | **`1.06x`** | `9,640` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.53`** | `10.51` | `10.72` | **`0.982x`** | `0.966x` | `28.22` | **`0.37x`** | `9,704` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.66`** | `38.37` | `38.59` | **`1.002x`** | `0.996x` | `50.27` | **`0.77x`** | `13,108` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`15.81`** | `15.59` | `16.55` | **`0.955x`** | `0.903x` | `8.00` | **`1.98x`** | `13,108` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.03`** | `11.84` | `12.02` | **`1.001x`** | `1.006x` | `18.93` | **`0.64x`** | `13,108` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.65`** | `11.50` | `30.81` | **`0.378x`** | `0.380x` | `11.04` | **`1.05x`** | `13,140` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.25`** | `45.38` | `48.20` | **`0.980x`** | `0.992x` | `57.46` | **`0.82x`** | `17,636` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.36`** | `39.73` | `40.93` | **`0.986x`** | `0.978x` | `24.42` | **`1.65x`** | `17,688` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.97`** | `4.75` | `4.84` | **`1.027x`** | `0.690x` | `4.06` | **`1.22x`** | `17,720` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.73`** | `33.48` | `36.65` | **`0.920x`** | `0.905x` | `26.63` | **`1.27x`** | `17,688` | `fc7490e2b80c68e2` ✓ |

---

## 03_a3_nokta_dml — `performans-A@946f61e`

**Adım 3 — satır kimliğiyle `UPDATE`/`DELETE` için özel planlar.** Hazırlanmış `UPDATE … WHERE rowid = ?` ve `DELETE … WHERE rowid = ?` genel VDBE yorumlayıcısından geçiyor, satır başına ikinci bir B-tree inişi ve tam kayıt çöz/yeniden kodla yapıyordu. `RowidPointUpdatePlan` / `RowidPointDeletePlan` tek ham arama yapıp değişmeyen alanları ham bayt olarak kopyalıyor; aynı indeks sil/ekle ve satır değiştir çağrılarını aynı baytlarla yapıyor. Yalnızca düz `main` tabloları için (tetikleyici, yabancı anahtar, `CHECK`, `STRICT`, üretilmiş sütun, kısmi/ifade indeksi, özel harmanlama, çakışma yan tümcesi, `RETURNING` yok); diğer her durumda VDBE programı çalışıyor. Ek olarak konumlu tablo işlemleri, ham yaprak yeniden paketleme ve bir kez çözülen indeks arama anahtarı (`RawRecordKey`). Hedef: `W08`; yan kazanç `W02`.

- **Testler**: `cargo test --workspace`: 524 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Rastgele farksal koşu (1.900 tohum, ~2,1 milyon ifade): çıktı, hata kodları, değişiklik sayaçları ve veritabanı baytları adım 2 ile aynı; tüm veritabanları C `integrity_check` geçiyor. Plan ortasında gelen G/Ç hatası / meşgul / kesme durumları hata enjeksiyonuyla denenmedi.
- **Toplam**: `281.43 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`0.970x`** (disk WAL `0.934x`, bellek-içi `1.113x`)
- **Tepe RSS**: `17,672 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.48`** | `7.44` | `7.46` | **`1.003x`** | `0.972x` | `9.65` | **`0.78x`** | `8,664` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.43` | `8.50` | **`0.883x`** | `0.806x` | `8.66` | **`0.87x`** | `9,500` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.66`** | `27.43` | `28.09` | **`0.985x`** | `0.755x` | `23.62` | **`1.17x`** | `9,500` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.69`** | `12.52` | `12.63` | **`1.005x`** | `0.876x` | `7.51` | **`1.69x`** | `9,564` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`18.02`** | `17.87` | `17.52` | **`1.028x`** | `1.043x` | `16.56` | **`1.09x`** | `9,564` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.64`** | `10.59` | `10.53` | **`1.011x`** | `0.976x` | `28.22` | **`0.38x`** | `9,696` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.76`** | `38.46` | `38.66` | **`1.003x`** | `0.998x` | `50.27` | **`0.77x`** | `11,876` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.49`** | `9.39` | `15.81` | **`0.600x`** | `0.542x` | `8.00` | **`1.19x`** | `11,876` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.69`** | `11.66` | `12.03` | **`0.971x`** | `0.977x` | `18.93` | **`0.62x`** | `11,876` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.18`** | `11.14` | `11.65` | **`0.959x`** | `0.365x` | `11.04` | **`1.01x`** | `12,060` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.88`** | `46.21` | `47.25` | **`0.992x`** | `0.984x` | `57.46` | **`0.82x`** | `17,476` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.81`** | `39.51` | `40.36` | **`1.011x`** | `0.989x` | `24.42` | **`1.67x`** | `17,516` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.91`** | `4.60` | `4.97` | **`0.988x`** | `0.682x` | `4.06` | **`1.21x`** | `17,548` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.73`** | `33.13` | `33.73` | **`1.000x`** | `0.905x` | `26.63` | **`1.27x`** | `17,516` | `fc7490e2b80c68e2` ✓ |

---

## 04_a4_kapsayan_indeks — `performans-A@40e275d`

**Adım 4 — kapsayan indeks taramalarında tamsayı çekirdeği.** `W04` ve `W05` zamanlarını hücre başına genel işte harcıyordu: ziyaret edilen her hücre için tam kayıt-aralık karşılaştırması, satır başına genel başlık ayrıştırma ve enum dağıtımlı `WHERE`/kümeleme değerlendirmesi. `IntRangeProbe` aralık hedefini arama başına bir kez çözüyor (tüm işlenenler `INTEGER` ve `DESC` sütun yoksa); `RawIndexLeafView` sayfa başına değişmezleri döngü dışına alıyor; `CovIntKernelShape` (`fast_read/int_kernel.rs`), `WHERE` sütun karşılaştırmaları/`BETWEEN` ve kümeler `count/sum/total/avg/min/max` olduğunda satırları tamsayı aralık testi + mevcut `FastSumAccum` ile işliyor (taşma anlambilimi aynı). Çekirdeğin çözemediği her satır ve tamsayı olmayan parametreli her yürütme mevcut genel koddan geçiyor. Hedef: `W04`, `W05`.

- **Testler**: `cargo test --workspace`: 527 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. Farksal koşular: 2.000 tohum (400 bin sorgu, yarısı bozuk indeks sayfalarıyla) ve 700 rastgele aralık/kümeleme sorgusu adım 3 ile birebir aynı; ikincisi ondalık biçimi normalleştirilince C ile de aynı. `W11` (+%3) ve `W09` (+%2,5) aynı komut sayısıyla yavaş okunuyor (kod yerleşimi; bu iş yüklerinin koduna dokunulmadı).
- **Toplam**: `262.85 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`0.844x`** (disk WAL `0.782x`, bellek-içi `1.116x`)
- **Tepe RSS**: `18,264 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.32`** | `7.28` | `7.48` | **`0.978x`** | `0.951x` | `9.65` | **`0.76x`** | `8,712` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.40` | `7.51` | **`1.000x`** | `0.806x` | `8.66` | **`0.87x`** | `9,552` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.47`** | `27.39` | `27.66` | **`0.993x`** | `0.750x` | `23.62` | **`1.16x`** | `9,552` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.73`** | `7.65` | `12.69` | **`0.609x`** | `0.534x` | `7.51` | **`1.03x`** | `9,616` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.40`** | `4.37` | `18.02` | **`0.244x`** | `0.255x` | `16.56` | **`0.27x`** | `9,616` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.60`** | `10.56` | `10.64` | **`0.996x`** | `0.972x` | `28.22` | **`0.38x`** | `9,808` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.49`** | `38.08` | `38.76` | **`0.993x`** | `0.991x` | `50.27` | **`0.77x`** | `13,264` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.53`** | `9.43` | `9.49` | **`1.005x`** | `0.544x` | `8.00` | **`1.19x`** | `13,264` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.77`** | `11.72` | `11.69` | **`1.007x`** | `0.984x` | `18.93` | **`0.62x`** | `13,264` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.99`** | `10.91` | `11.18` | **`0.983x`** | `0.359x` | `11.04` | **`1.00x`** | `13,232` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`49.01`** | `47.48` | `46.88` | **`1.045x`** | `1.029x` | `57.46` | **`0.85x`** | `18,116` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.00`** | `39.29` | `40.81` | **`0.980x`** | `0.969x` | `24.42` | **`1.64x`** | `18,148` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.85`** | `4.54` | `4.91` | **`0.989x`** | `0.674x` | `4.06` | **`1.19x`** | `18,180` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.17`** | `33.08` | `33.73` | **`0.983x`** | `0.890x` | `26.63` | **`1.25x`** | `18,148` | `fc7490e2b80c68e2` ✓ |

---

## 05_a5_rtree — `performans-A@700ded4`

**Adım 5 — R-Tree yerinde düğüm güncelleme ve sanal tablo işlem sırasının belirlenimci yapılması.** `W12`nin süresi disk G/Ç'si değil CPU işiydi: bölünmeyen eklemelerde ve üst düğüm sınır kutusu düzeltmesinde düğüm `klonla → değiştir → geri koy` yerine yerinde değiştiriliyor, tam düğüm anlık görüntüsü yerine küçük geri alma kayıtları tutuluyor; düğüm sınır kutusu önbelleği, tek sorgulu önbellek erişimi, düğüm numaralı haritalar için ucuz hasher; `SELECT`in `WHERE` ifadesi yürütme başına 4 kez derin kopyalanıyordu, artık ödünç alınıyor. Gölge tablo baytları değişmedi. **Belirlenimsizlik düzeltmesi (tabanda da vardı, doğrulama sırasında bulundu):** aynı işlemde iki veya daha çok sanal tablo değişince dosya, süreç başına rastgele seçilen birkaç bayt-farklı varyanttan biri oluyordu (işlem yayılımı rastgele tohumlu `HashMap` sırasıyla yürüyordu). Artık tablolar işleme katılma sırasıyla işleniyor (C'deki `db->aVTrans` sırası). Mantıksal içerik hiç etkilenmiyordu; fark yalnızca gölge tablolar arasındaki sayfa tahsis sırasındaydı. Hedef: `W12`; yan kazanç `W11`.

- **Testler**: `cargo test --workspace`: 533 geçti (6 yeni), bilinen 4 başarısız, 337 yok sayıldı. Çok sanal tablolu iki tekrar betiği 16'şar koşuda tek dosya üretiyor (önceden iki); çıktı adım 4 ile aynı, C `rtreecheck` + `integrity_check` `ok`. Farksal koşu: 650 tek R-Tree tohumu çıktı ve dosya baytlarında adım 4 ile aynı; 300 çok R-Tree tohumu çıktıda aynı ve koşudan koşuya kendisiyle aynı. Dokunulmayan bilinen hatalar (tabanda var): 512 bayt sayfalı bazı R-Tree dosyalarında C `integrity_check` → `Child page depth differs`; R-Tree satır kimliği tahsisi `i64::MAX`ta taşıyor. Ayrıntı: `saglik/c_uyumluluk_ve_saglik_analizi.md`.
- **Toplam**: `242.95 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`0.808x`** (disk WAL `0.782x`, bellek-içi `0.909x`)
- **Tepe RSS**: `17,912 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.44`** | `7.42` | `7.32` | **`1.017x`** | `0.967x` | `9.65` | **`0.77x`** | `8,908` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.59`** | `7.54` | `7.51` | **`1.010x`** | `0.815x` | `8.66` | **`0.88x`** | `9,748` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.00`** | `27.64` | `27.47` | **`1.019x`** | `0.765x` | `23.62` | **`1.19x`** | `9,748` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.95`** | `7.75` | `7.73` | **`1.028x`** | `0.549x` | `7.51` | **`1.06x`** | `9,812` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.31`** | `4.30` | `4.40` | **`0.979x`** | `0.249x` | `16.56` | **`0.26x`** | `9,812` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.43`** | `10.34` | `10.60` | **`0.984x`** | `0.957x` | `28.22` | **`0.37x`** | `9,940` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.41`** | `38.11` | `38.49` | **`0.998x`** | `0.989x` | `50.27` | **`0.76x`** | `12,420` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.39`** | `9.38` | `9.53` | **`0.985x`** | `0.536x` | `8.00` | **`1.17x`** | `12,420` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.79`** | `11.76` | `11.77` | **`1.002x`** | `0.985x` | `18.93` | **`0.62x`** | `12,420` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.01`** | `10.91` | `10.99` | **`1.002x`** | `0.359x` | `11.04` | **`1.00x`** | `12,672` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.06`** | `44.72` | `49.01` | **`0.919x`** | `0.946x` | `57.46` | **`0.78x`** | `17,684` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.45`** | `22.99` | `40.00` | **`0.586x`** | `0.568x` | `24.42` | **`0.96x`** | `17,748` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.78`** | `4.51` | `4.85` | **`0.984x`** | `0.663x` | `4.06` | **`1.18x`** | `17,780` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.36`** | `33.02` | `33.17` | **`1.006x`** | `0.895x` | `26.63` | **`1.25x`** | `17,748` | `fc7490e2b80c68e2` ✓ |

---

## 05b_duzeltme_dengesiz_agac — `performans-A@1c12e74`

**Adım 5b — doğruluk düzeltmesi (performans amacı yok).** Üç hata da ilk port commit'inden (`814e41e`) beri vardı. (1) **Dengesiz tablo B-tree'si:** silme, kök olmayan bir iç sayfayı tek çocukla bırakınca o sayfa serbest bırakılıp üst düğüm doğrudan toruna bağlanıyordu; alt ağaç kardeşlerinden bir seviye kısa kalıyordu. C bunu bozulma sayıyor (`Child page depth differs`), motorun kendisi de sonradan aynı ağaçta hata verip satır kaybedebiliyordu. Artık sayfa korunup aynı seviyedeki kardeşleriyle C'deki gibi yeniden dengeleniyor; derinlik yalnızca kökte değişiyor. (2) **İndeks sayfasının hücre sayısına göre bölünmesi:** çok farklı boyutlu anahtarlarda bir yarı sayfaya sığmıyor ve `INSERT` `cells exceed usable B-tree page capacity` ile düşüyordu (C'de başarılı); bölünme noktası artık iki yarının da sığdığı en yakın konum (önceden çalışan bölünmeler bayt bayt aynı). (3) **`PRAGMA integrity_check`:** C'nin `btree.c` kontrolleri C'nin mesaj metinleriyle port edildi — B-tree düzeyinde 30 kontrolün 29'u (önce 6), SQL düzeyinde 24'ün 18'i (önce 2) C ile aynı. Ayrıntı: `saglik/integrity_check_parity.md`.

- **Testler**: `cargo test --workspace`: 542 geçti (9 yeni), bilinen 4 başarısız, 337 yok sayıldı. Üç tekrar betiği artık C `integrity_check` + `rtreecheck` geçiyor ve ağaç başına tek yaprak derinliği var; yeni Rust kontrolü düzeltme öncesi yazılmış bozuk dosyalarda C ile aynı mesajı veriyor. Hataya hiç girmeyen 13 senaryonun dosyaları önceki commit ile bayt bayt aynı. Farksal koşu: 12.960 + 1.920 çalıştırma, her dosya C `integrity_check` geçiyor. **Hız:** `W01`–`W13` komut sayıları değişmedi (hiçbir iş yükü düzeltilen yollara girmiyor; önceki kazançların hiçbiri bu hatalara dayanmıyordu). `W14` +%5: betik `PRAGMA integrity_check` ile bitiyor ve kontrol artık C'nin yaptığı işi yapıyor (eski 2,0 M komut, C 5,4 M, yeni 14,4 M). Doğrulanmayan: commit bazında ikiye bölme; `auto_vacuum` + iç sayfa yeniden dengeleme için ayrı C farksalı; kasıtlı bozulmuş 6.000 dosyanın 5'inde C hata verirken motor vermiyor (sayfa 1 hasarını şema yükleyici kabul ediyor).
- **Toplam**: `244.11 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`0.807x`** (disk WAL `0.781x`, bellek-içi `0.912x`)
- **Tepe RSS**: `17,868 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.51`** | `7.45` | `7.44` | **`1.009x`** | `0.976x` | `9.65` | **`0.78x`** | `8,732` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.50`** | `7.39` | `7.59` | **`0.988x`** | `0.805x` | `8.66` | **`0.86x`** | `9,636` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.82`** | `27.45` | `28.00` | **`0.993x`** | `0.760x` | `23.62` | **`1.18x`** | `9,636` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.81`** | `7.72` | `7.95` | **`0.983x`** | `0.539x` | `7.51` | **`1.04x`** | `9,700` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.09`** | `4.08` | `4.31` | **`0.951x`** | `0.237x` | `16.56` | **`0.25x`** | `9,700` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.66`** | `10.61` | `10.43` | **`1.023x`** | `0.979x` | `28.22` | **`0.38x`** | `9,892` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.30`** | `38.12` | `38.41` | **`0.997x`** | `0.986x` | `50.27` | **`0.76x`** | `12,372` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.38`** | `9.34` | `9.39` | **`0.999x`** | `0.536x` | `8.00` | **`1.17x`** | `12,372` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.98`** | `11.94` | `11.79` | **`1.016x`** | `1.002x` | `18.93` | **`0.63x`** | `12,372` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.06`** | `10.99` | `11.01` | **`1.004x`** | `0.361x` | `11.04` | **`1.00x`** | `12,620` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`44.94`** | `44.53` | `45.06` | **`0.997x`** | `0.943x` | `57.46` | **`0.78x`** | `17,744` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.63`** | `23.17` | `23.45` | **`1.008x`** | `0.573x` | `24.42` | **`0.97x`** | `17,808` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.68`** | `4.53` | `4.78` | **`0.981x`** | `0.650x` | `4.06` | **`1.15x`** | `17,840` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`34.74`** | `34.51` | `33.36` | **`1.041x`** | `0.932x` | `26.63` | **`1.30x`** | `17,808` | `fc7490e2b80c68e2` ✓ |

---

## 05d_duzeltme_wal_cokme — `performans-A@3707ece`

**Adım 5d — doğruluk düzeltmesi: WAL çökme atomikliği (performans amacı yok).** 5c envanterinde bulundu; WAL yazıcısı port edildiğinden beri vardı. Büyük bir yazma işleminde sayfa önbelleği taşıp sayfalar commit'ten önce `-wal`e yazıldığında ve taşmış bir sayfa yeniden değiştiğinde, `COMMIT` eski çerçeveyi yerinde yeniden yazar ve sonuna (commit çerçevesi dahil) yeni çerçeveler ekler. C bu sırada eklenen çerçeveleri tuz ve sağlama toplamı yerine 16 sıfır baytla yazar (`walEncodeFrame`, `iReCksum != 0`); geçerli zinciri ancak `walRewriteChecksums` kurar. Motor bu çerçeveleri gerçek tuz ve eski görüntüler üzerinden zincirlenmiş sağlama toplamıyla yazıyordu: çökme, eklenen çerçeveleri koruyup yerinde yeniden yazımı kaybederse **yarım uygulanmış bir işlem** görünür oluyordu (C aynı durumda işlemi atar). Artık C ile aynı: bekleyen yeniden yazım varken eklenen çerçeveler doğrulanamaz biçimde yazılıyor. Yazma sırası ve adım 1'deki çerçeve toplama değişmedi.

- **Testler**: `cargo test --workspace`: 546 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Dört yeni test, senkronlanmamış `-wal` yazımlarının her alt kümesini (test başına ~1.000–4.100 çökme görüntüsü) dener; kurtarma ya işlem öncesi durumu ya da işlemin tamamını vermeli. Düzeltme geri alınınca dördü de düşüyor. Süreç öldürme + blok geri yükleme betiği: düzeltmeden önce 600 satır ve iki güncellemeden biri uygulanmış; sonra C yazıcısıyla aynı sonuç (işlem atılıyor). **Hız:** toplam 0,999x; ölçüm iş yükleri önbellek taşırmadığı için `W13`/`W01` komut ve sistem çağrısı sayıları aynı. Doğrulanmayan: gerçek güç kesintisi (tüm çökme durumları sentetik). Kod okunarak bulunan ve düzeltilmeyen iki ara-durum farkı daha var (`journal_size_limit` kırpma hatası; `load_deserialized_image` içinde senkronsuz kapanış checkpoint'i).
- **Toplam**: `245.45 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`0.810x`** (disk WAL `0.782x`, bellek-içi `0.921x`)
- **Tepe RSS**: `17,928 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.47`** | `7.43` | `7.51` | **`0.995x`** | `0.971x` | `9.65` | **`0.77x`** | `8,988` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.54`** | `7.45` | `7.50` | **`1.005x`** | `0.809x` | `8.66` | **`0.87x`** | `9,824` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.73`** | `27.65` | `27.82` | **`0.997x`** | `0.757x` | `23.62` | **`1.17x`** | `9,824` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.87`** | `7.78` | `7.81` | **`1.008x`** | `0.544x` | `7.51` | **`1.05x`** | `9,888` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.10`** | `4.09` | `4.09` | **`1.001x`** | `0.237x` | `16.56` | **`0.25x`** | `9,888` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.80`** | `10.78` | `10.66` | **`1.013x`** | `0.991x` | `28.22` | **`0.38x`** | `10,020` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.45`** | `38.09` | `38.30` | **`1.004x`** | `0.990x` | `50.27` | **`0.77x`** | `12,564` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.35`** | `9.34` | `9.38` | **`0.997x`** | `0.534x` | `8.00` | **`1.17x`** | `12,564` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.95`** | `11.89` | `11.98` | **`0.997x`** | `0.999x` | `18.93` | **`0.63x`** | `12,564` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.07`** | `11.04` | `11.06` | **`1.002x`** | `0.362x` | `11.04` | **`1.00x`** | `12,876` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.39`** | `44.40` | `44.94` | **`1.010x`** | `0.953x` | `57.46` | **`0.79x`** | `17,864` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.10`** | `23.40` | `23.63` | **`1.020x`** | `0.584x` | `24.42` | **`0.99x`** | `17,892` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.72`** | `4.54` | `4.68` | **`1.007x`** | `0.655x` | `4.06` | **`1.16x`** | `17,924` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`34.89`** | `34.50` | `34.74` | **`1.004x`** | `0.936x` | `26.63` | **`1.31x`** | `17,892` | `fc7490e2b80c68e2` ✓ |

---

## 05e_duzeltme_guvence — `performans-A@e53008f`

**Adım 5e — doğruluk düzeltmeleri: güvence ve uyumluluk hataları (performans amacı yok).** (1) `PRAGMA secure_delete` yok sayılıyordu (satır dönmüyor, silinen hücrelerin içeriği dosyada kalıyordu); tüm biçimleriyle uygulandı, `ON` iken dosyada yalnızca canlı satırların baytları kalıyor. (2) C'de satır dönen ama burada dönmeyen pragmalar: `analysis_limit` (saklanıyor ve `ANALYZE` tarafından uygulanıyor), `data_version`, `cell_size_check` (yüklenen sayfalar C'deki gibi doğrulanıyor), `mmap_size`. (3) `ALTER TABLE ADD COLUMN` sütun metnini AST'den yeniden üretiyordu: `DEFAULT x'00'` → `x''` (varsayılan değer **kayboluyordu**), sabit olmayan varsayılan kabul ediliyordu; artık kullanıcının metni C'deki gibi aynen ekleniyor, C'nin kontrolleri C'nin sırası ve mesajlarıyla çalışıyor. (4) Tablosu olmayan veritabanının başlığı: şema biçimi ve kodlama ilk şema satırına kadar 0 kalıyor ve `PRAGMA encoding` o ana kadar geçerli. (5) İki WAL hata yolu: `journal_size_limit` kırpma hatası artık commit çerçevesi kalıcı olmuş bir `COMMIT`i başarısız yapmıyor; `load_deserialized_image` WAL'i senkronlu checkpoint ile kapatıyor.

- **Testler**: `cargo test --workspace`: 559 geçti (13 yeni), bilinen 4 başarısız, 337 yok sayıldı. `ADD COLUMN` farksalı 1.126 durumda C ile eşit: 1.118 (önce 407). Ayrışma bulucu (192 senaryo): hiçbir senaryo daha kötü katmanda bitmiyor, 11'i iyileşti. 13 düzen senaryosu önceki commit ile bayt bayt aynı. **Hız:** toplam 0,998x; komut sayıları aynı (`W08`/`W09` +%0,05). `W05` +%5 aynı komut sayısıyla (kod yerleşimi). **Bilerek C'den farklı bırakılan (mevcut entegrasyon testleri eski davranışı doğruluyor):** `ADD COLUMN` satır kontrolü hatası `SQLITE_CONSTRAINT` dönüyor (C: `SQLITE_ERROR`); `too many columns on t` (C: `… on sqlite_altertab_t`). Doğrulanmayan: deserialize yolu çalışan bir C ikilisine karşı.
- **Toplam**: `246.08 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`0.812x`** (disk WAL `0.784x`, bellek-içi `0.923x`)
- **Tepe RSS**: `17,836 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.40`** | `7.35` | `7.47` | **`0.991x`** | `0.962x` | `9.65` | **`0.77x`** | `8,920` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.53`** | `7.50` | `7.54` | **`0.999x`** | `0.808x` | `8.66` | **`0.87x`** | `9,756` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.51`** | `27.35` | `27.73` | **`0.992x`** | `0.751x` | `23.62` | **`1.17x`** | `9,756` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.76`** | `7.63` | `7.87` | **`0.986x`** | `0.536x` | `7.51` | **`1.03x`** | `9,820` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.31`** | `4.29` | `4.10` | **`1.051x`** | `0.249x` | `16.56` | **`0.26x`** | `9,820` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.60` | `10.80` | **`0.985x`** | `0.976x` | `28.22` | **`0.38x`** | `10,016` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.79`** | `38.60` | `38.45` | **`1.009x`** | `0.999x` | `50.27` | **`0.77x`** | `12,560` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.41`** | `9.37` | `9.35` | **`1.006x`** | `0.537x` | `8.00` | **`1.18x`** | `12,624` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.83`** | `11.80` | `11.95` | **`0.990x`** | `0.989x` | `18.93` | **`0.62x`** | `12,624` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.09`** | `11.05` | `11.07` | **`1.001x`** | `0.362x` | `11.04` | **`1.00x`** | `12,872` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.63`** | `44.36` | `45.39` | **`1.005x`** | `0.958x` | `57.46` | **`0.79x`** | `17,716` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.12`** | `23.29` | `24.10` | **`1.001x`** | `0.585x` | `24.42` | **`0.99x`** | `17,776` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.73`** | `4.47` | `4.72` | **`1.001x`** | `0.656x` | `4.06` | **`1.16x`** | `17,808` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.35`** | `34.70` | `34.89` | **`1.013x`** | `0.948x` | `26.63` | **`1.33x`** | `17,776` | `fc7490e2b80c68e2` ✓ |

---

## 05f_duzeltme_fts5 — `performans-A@e4cdaff`

**Adım 5f — doğruluk düzeltmesi: FTS5'in C ile iki yönlü uyumu (performans amacı yok).** Bu değişiklikten önce iki motor bir FTS5 tablosunu güvenle paylaşamıyordu (farksal koşuda 300 tohumun 300'ü en az bir yönde başarısızdı). Düzeltilenler: **silme bayrağı** (okuma: C'nin `UPDATE`inden sonra `MATCH` satır kaçırıyor, ardından `'optimize'` o kayıtları kalıcı siliyordu; yazma: aynı boşaltmada silinip yeniden eklenen satırın bayrağı kayboluyordu); **64 KiB'ı aşan yaprak** (C: bozuk); **`contentless_delete=1`** kayıt biçimi; **ayraçlar** (unicode61, ascii, porter, trigram C'den bayt düzeyinde port edildi — katlama/aksan/kök bulma farklıydı, C `checksum mismatch` veriyordu); **çakışma yan tümceleri** (`INSERT OR REPLACE` vb.); **`contentless_unindexed=1`, `locale=1`**; FTS5 içinde gerçel→metin çevrimi. Ayrıca sorgu dilbilgisi, `bm25`/`rank`, `highlight`, `snippet`, `fts5vocab`, özel komutlar ve hata metinleri C'ye getirildi. **Kapsam dışı (yapısal, ayrı karar):** segment birleştirme takvimi ve boşaltma zamanı, yaprak boyutu hedefi ve doclist indeksi, yapı çerezi.

- **Testler**: `cargo test --workspace`: 597 geçti (38 yeni; 34'ü C CLI'sinden üretilmiş, gömülü C gölge tablo görüntüleriyle), bilinen 4 başarısız, 337 yok sayıldı. Yeni 34 durumun 32'si önceki kaynakta düşüyor. Farksal koşu (rastgele yapılandırma, işlemler ve sorgular; Rust yazar / C yazar / sırayla yazar; dört motor-dosya birleşiminde bütünlük kontrolü): 3.600 tohum / 10.800 çalıştırma, 0 hata (önce 900 çalıştırmanın 863'ü). 60 tohum yalnızca `secure-delete=1` ile farklı; her biri C'nin kendi indeksini bozmasına dayanıyor (51'i yalnız C'nin yazdığı geçmişte). **Hız:** toplam 1,004x; `W11` +%2,6 (komut sayısı 248,20 M → 247,86 M; `bm25`in C ile birebir aynı olması için eşleşen satır başına daha çok bellek). Düzeltilmeyen: `fts5_locale()` yok; `LIMIT` ötesindeki satırlar için yardımcı fonksiyon değerlendirmesi; `%_config` sürümü bağlanırken denetlenmiyor.
- **Toplam**: `246.19 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`0.814x`** (disk WAL `0.787x`, bellek-içi `0.921x`)
- **Tepe RSS**: `18,156 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.42`** | `7.41` | `7.40` | **`1.002x`** | `0.964x` | `9.65` | **`0.77x`** | `8,996` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.60`** | `7.54` | `7.53` | **`1.010x`** | `0.816x` | `8.66` | **`0.88x`** | `9,832` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.57`** | `27.47` | `27.51` | **`1.002x`** | `0.753x` | `23.62` | **`1.17x`** | `9,832` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.76`** | `7.66` | `7.76` | **`1.001x`** | `0.536x` | `7.51` | **`1.03x`** | `9,896` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.40`** | `4.38` | `4.31` | **`1.022x`** | `0.255x` | `16.56` | **`0.27x`** | `9,960` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.44`** | `10.39` | `10.63` | **`0.982x`** | `0.958x` | `28.22` | **`0.37x`** | `10,028` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.53`** | `38.36` | `38.79` | **`0.993x`** | `0.993x` | `50.27` | **`0.77x`** | `12,508` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.43`** | `9.37` | `9.41` | **`1.002x`** | `0.538x` | `8.00` | **`1.18x`** | `12,508` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.97`** | `11.69` | `11.83` | **`1.012x`** | `1.001x` | `18.93` | **`0.63x`** | `12,508` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.08`** | `11.06` | `11.09` | **`0.999x`** | `0.362x` | `11.04` | **`1.00x`** | `12,692` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.91`** | `45.22` | `45.63` | **`1.006x`** | `0.964x` | `57.46` | **`0.80x`** | `17,956` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.76`** | `23.08` | `24.12` | **`0.985x`** | `0.576x` | `24.42` | **`0.97x`** | `18,020` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.79`** | `4.56` | `4.73` | **`1.014x`** | `0.666x` | `4.06` | **`1.18x`** | `18,052` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.52`** | `35.14` | `35.35` | **`1.005x`** | `0.953x` | `26.63` | **`1.33x`** | `18,020` | `fc7490e2b80c68e2` ✓ |

---

## 05g_hizalama_dosya — `performans-A@fc4f0ca`

**Adım 5g — C'ye hizalama: dosya düzeyi, bedelsiz sınıflar.** Aynı ifade dizisi için yazılan dosyayı C'nin yazdığına yaklaştırır. (1) **Değişim sayacı** 0'dan başlıyor ve WAL commit'inde C gibi sabit kalıyor. (2) **Şema çerezi** satır başına değil ifade başına artıyor; `PRAGMA schema_version` her ifadeden sonra C ile aynı. (3) **Satır sıraları**: sanal tablo ve gölge tablolarının `sqlite_schema` sırası, `ANALYZE`in `sqlite_stat1` sırası, `CREATE TABLE … UNIQUE` yer tutucu satır dizisi, `VACUUM` ve `DROP TABLE` sırası. (4) **`REAL` sütunda tam değerli sayı** C'nin kuralıyla tamsayı olarak saklanıyor (dosya 1,8 kat küçülüyor); okuma tarafı da düzeltildi (C'nin yazdığı dosyada bu değerler bazı yollarda `integer` dönüyordu). `jsonb_set` vb. metni C'nin eleman türüyle yazıyor. (5) **Serbest bırakılan sayfalar** artık günlüğe alınmıyor, sıfırlanmıyor ve yeniden yazılmıyor (`DROP TABLE` 6.260 → 13 yazma çağrısı; C 25); `secure_delete` `ON`/`FAST`/`OFF` sayfa kararları C ile aynı. (6) **Serbest liste sırası** (ilk gövdenin ilk yaprağı / en yakın yaprak) ve **satır başına yazma sırası** (önce indeksler, sonra tablo satırı; `UPDATE`te yalnızca C'nin yeniden yazdığı indeksler). (7) Günlük başlığı 512 bayt; kısmi checkpoint C'nin sayfa seçimi; yeni dosyalar 0644 ve yan dosyalar veritabanının iznini alıyor. İki ek bulgu düzeltildi: iki `UNIQUE` ihlalinde hata sırası C'ninki oldu.

- **Testler**: `cargo test --workspace`: 626 geçti (29 yeni; 19'u C'nin yazdığı dosyayla bayt eşitliği), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu (192 senaryo):** hiçbir senaryo daha kötü katmanda bitmiyor; **22'si artık C ile bayt bayt aynı (önce 0)**, 42'si yalnızca kullanılmayan baytlarda farklı. **Karar eşliği** (izli C derlemesi ile Rust karar kayıtlarının olay olay karşılaştırması, 234 betik): çerez 234/234, `REAL` saklama 234/234, JSONB 234/234, şema satırları 227/234, serbest bırakma 181/234, tahsis 191/234, yazma sırası 170/234 — son üçü ağaç şekline bağlı; uyuşmayan betikler kapsam dışı sınıfların (silmede yeniden dengeleme, indeks doluluğu, bölünmeler, FTS5/R-Tree yazıcıları) ifadelerinde ayrışıyor. Ayrıntı: `saglik/karar_esligi_5g.md`. **Çökme güvenliği:** 258 geri alma betiği ve commit sırasında öldürülen 2.592 kurtarma (iki yazıcı × iki kurtarıcı), 0 hata (tmpfs; gerçek güç kesintisi yok). **Hız:** toplam 0,998x; en büyük komut artışı `W10` +%0,96. Hizalanmayan/doğrulanmayan: serbest listeden alınan sayfa hâlâ okunup sıfırlanıyor; indeks taramasıyla `DELETE` ve var olan satır kimliğinde `REPLACE` yazma sırası; `FAST` modunda dosyadaki artık C'den biraz fazla (nedeni incelenmedi); `PRAGMA optimize` yok.
- **Toplam**: `247.29 ms` (taban `328.62 ms`, C `295.04 ms`) — geomean `Rust/C` **`0.816x`** (disk WAL `0.788x`, bellek-içi `0.925x`)
- **Tepe RSS**: `18,112 KiB` (C: `6,176 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.56`** | `7.53` | `7.42` | **`1.019x`** | `0.983x` | `9.65` | **`0.78x`** | `8,984` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.53`** | `7.45` | `7.60` | **`0.991x`** | `0.808x` | `8.66` | **`0.87x`** | `9,824` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.97`** | `27.54` | `27.57` | **`1.015x`** | `0.764x` | `23.62` | **`1.18x`** | `9,824` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.72`** | `7.70` | `7.76` | **`0.995x`** | `0.533x` | `7.51` | **`1.03x`** | `9,888` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.38`** | `4.36` | `4.40` | **`0.995x`** | `0.253x` | `16.56` | **`0.26x`** | `9,888` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.48`** | `10.44` | `10.44` | **`1.004x`** | `0.962x` | `28.22` | **`0.37x`** | `9,952` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.92`** | `38.59` | `38.53` | **`1.010x`** | `1.002x` | `50.27` | **`0.77x`** | `12,276` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.47`** | `9.41` | `9.43` | **`1.004x`** | `0.541x` | `8.00` | **`1.18x`** | `12,276` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.88`** | `11.85` | `11.97` | **`0.993x`** | `0.993x` | `18.93` | **`0.63x`** | `12,276` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.16`** | `11.10` | `11.08` | **`1.007x`** | `0.364x` | `11.04` | **`1.01x`** | `12,524` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.63`** | `45.49` | `45.91` | **`1.016x`** | `0.979x` | `57.46` | **`0.81x`** | `18,004` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.60`** | `23.23` | `23.76` | **`0.994x`** | `0.572x` | `24.42` | **`0.97x`** | `18,080` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.81`** | `4.55` | `4.79` | **`1.003x`** | `0.668x` | `4.06` | **`1.18x`** | `18,112` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.17`** | `34.61` | `35.52` | **`0.990x`** | `0.943x` | `26.63` | **`1.32x`** | `18,080` | `fc7490e2b80c68e2` ✓ |
