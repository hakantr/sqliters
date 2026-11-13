# `performans-A` — Saf Rust Optimizasyon Adımları Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-13T08:20:51+03:00`
- **Yöntem**: `speedbench` `W01`–`W14`; 1 ısınma + 9 sıralı (interleaved) deneme, medyan; süreçler `taskset -c 20` ile tek çekirdeğe sabit; veritabanı dosyaları `tmpfs (/run/user/1000/sqliters-perf)` üzerinde (fsync gürültüsü yok). Tüm aşamaların saklanmış ikilileri ve C referansı **aynı oturumda** ölçüldü.
- **Kısıtlar (tüm aşamalar)**: `#![forbid(unsafe_code)]`, sıfır dış bağımlılık, SWAR / SIMD / `asm!` yok, SQLite Format-3 ve WAL protokolü uyumu, C ile birebir `FNV-1a` çıktı özeti.

> **Dikkat — farklı donanım.** Üst dizindeki `00`–`03` anlık görüntüleri ve `benchmark_report.md` başka bir makinede (`AMD Ryzen 7 8845HS`) ölçülmüştür; bu dizindeki sayılar o tabloyla doğrudan karşılaştırılamaz. Buradaki `00_performans_taban` satırı, aynı `performans@4ccdc64` kodunun **bu makinedeki** ölçümüdür ve `performans-A` adımlarının bazıdır.
>
> **Ölçülmeyen.** `./sqltest` uçtan uca CLI komut seti (126 örnek) ve C `rss_probe` tabanlı çocuk süreç RSS ölçümü bu aşamalarda koşulmadı; JSON'larda `sqltest_cli_command_set` alanı `null`.

**Dosyalar**: her aşama için `NN_<aşama>.json` (deneme bazında süreler, RSS, özetler); `asamalar.json` (aşama tanımları, commit'ler, test sonuçları); `olc.py` (bu raporu üreten betik); `ek_resmi_run_speedbench_taban.json` (bazın `tools/run_speedbench.py` ile sabitlenmemiş, btrfs üzerindeki ilk resmi koşusu: Rust `723,80 ms` / C `647,26 ms`, geomean `1,12x` — fsync ve zamanlama gürültüsü içerir).

## 1. Aşama Özeti

| Aşama | Git (`dal@commit`) | İkili (`KiB`) | `.text` (`KiB`) | Toplam (`ms`) | Geomean (`vs taban`) | Geomean (`vs C`) | Toplam (`vs C`) | Tepe RSS (`KiB`) | Özetler |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`295.63`** | `—` | **`1.000x`** | **`1.000x`** | `6,368` | `—` |
| **00_performans_taban** | `performans@4ccdc64` | `6,382.7` | `5,412.9` | **`328.91`** | `1.000x` | **`1.184x`** | `1.113x` | `17,900` | 14/14 ✓ |
| **01_a1_wal_islem** | `performans-A@693ac8b` | `6,388.4` | `5,414.5` | **`314.98`** | `0.938x` | **`1.110x`** | `1.065x` | `17,904` | 14/14 ✓ |
| **02_a2_jsonb_indeks** | `performans-A@e2f7843` | `6,417.4` | `5,438.1` | **`290.03`** | `0.860x` | **`1.018x`** | `0.981x` | `17,696` | 14/14 ✓ |
| **03_a3_nokta_dml** | `performans-A@946f61e` | `6,456.6` | `5,472.7` | **`281.62`** | `0.818x` | **`0.969x`** | `0.953x` | `17,652` | 14/14 ✓ |
| **04_a4_kapsayan_indeks** | `performans-A@40e275d` | `6,644.6` | `5,647.8` | **`263.31`** | `0.714x` | **`0.846x`** | `0.891x` | `18,228` | 14/14 ✓ |
| **05_a5_rtree** | `performans-A@700ded4` | `6,650.1` | `5,652.7` | **`245.12`** | `0.685x` | **`0.811x`** | `0.829x` | `17,892` | 14/14 ✓ |
| **05b_duzeltme_dengesiz_agac** | `performans-A@1c12e74` | `6,717.4` | `5,713.6` | **`246.74`** | `0.686x` | **`0.813x`** | `0.835x` | `17,924` | 14/14 ✓ |
| **05d_duzeltme_wal_cokme** | `performans-A@3707ece` | `6,717.5` | `5,713.7` | **`246.75`** | `0.688x` | **`0.815x`** | `0.835x` | `17,884` | 14/14 ✓ |
| **05e_duzeltme_guvence** | `performans-A@e53008f` | `6,734.0` | `5,727.9` | **`246.27`** | `0.687x` | **`0.813x`** | `0.833x` | `17,976` | 14/14 ✓ |
| **05f_duzeltme_fts5** | `performans-A@e4cdaff` | `6,751.0` | `5,734.4` | **`247.16`** | `0.689x` | **`0.816x`** | `0.836x` | `18,152` | 14/14 ✓ |
| **05g_hizalama_dosya** | `performans-A@fc4f0ca` | `6,802.1` | `5,778.2` | **`248.30`** | `0.691x` | **`0.819x`** | `0.840x` | `18,092` | 14/14 ✓ |
| **05h_hizalama_sql** | `performans-A@a0cffa4` | `6,883.8` | `5,850.2` | **`249.18`** | `0.693x` | **`0.820x`** | `0.843x` | `18,280` | 14/14 ✓ |
| **05i_duzeltme_yedekleme** | `performans-A@39deab5` | `6,884.3` | `5,850.6` | **`249.04`** | `0.695x` | **`0.823x`** | `0.842x` | `18,268` | 14/14 ✓ |
| **05j_hizalama_cli_yedekleme** | `performans-A@225f5c2` | `7,299.0` | `6,157.8` | **`248.48`** | `0.692x` | **`0.819x`** | `0.841x` | `18,224` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_taban | 01_a1_wal_islem | 02_a2_jsonb_indeks | 03_a3_nokta_dml | 04_a4_kapsayan_indeks | 05_a5_rtree | 05b_duzeltme_dengesiz_agac | 05d_duzeltme_wal_cokme | 05e_duzeltme_guvence | 05f_duzeltme_fts5 | 05g_hizalama_dosya | 05h_hizalama_sql | 05i_duzeltme_yedekleme | 05j_hizalama_cli_yedekleme |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.63` | `7.73` (**0.80x**) | `7.45` (**0.77x**) | `7.43` (**0.77x**) | `7.48` (**0.78x**) | `7.35` (**0.76x**) | `7.47` (**0.78x**) | `7.57` (**0.79x**) | `7.53` (**0.78x**) | `7.48` (**0.78x**) | `7.53` (**0.78x**) | `7.63` (**0.79x**) | `7.71` (**0.80x**) | `7.67` (**0.80x**) | `7.58` (**0.79x**) |
| `W02_create_indexes` | `8.74` | `9.34` (**1.07x**) | `9.11` (**1.04x**) | `8.57` (**0.98x**) | `7.55` (**0.86x**) | `7.56` (**0.86x**) | `7.68` (**0.88x**) | `7.47` (**0.85x**) | `7.63` (**0.87x**) | `7.63` (**0.87x**) | `7.66` (**0.88x**) | `7.54` (**0.86x**) | `7.68` (**0.88x**) | `7.77` (**0.89x**) | `7.57` (**0.86x**) |
| `W03_pk_point_select` | `23.35` | `37.02` (**1.58x**) | `27.88` (**1.19x**) | `27.80` (**1.19x**) | `27.71` (**1.19x**) | `27.97` (**1.20x**) | `27.86` (**1.19x**) | `28.02` (**1.20x**) | `28.11` (**1.20x**) | `27.63` (**1.18x**) | `27.66` (**1.18x**) | `27.69` (**1.19x**) | `27.81` (**1.19x**) | `27.84` (**1.19x**) | `28.22` (**1.21x**) |
| `W04_secondary_idx_range` | `7.47` | `14.35` (**1.92x**) | `12.57` (**1.68x**) | `12.71` (**1.70x**) | `12.70` (**1.70x**) | `7.91` (**1.06x**) | `7.84` (**1.05x**) | `7.92` (**1.06x**) | `7.95` (**1.06x**) | `7.71` (**1.03x**) | `7.82` (**1.05x**) | `7.84` (**1.05x**) | `7.78` (**1.04x**) | `7.78` (**1.04x**) | `7.89` (**1.06x**) |
| `W05_full_scan_filter_agg` | `16.59` | `17.28` (**1.04x**) | `17.89` (**1.08x**) | `17.52` (**1.06x**) | `18.09` (**1.09x**) | `4.41` (**0.27x**) | `4.33` (**0.26x**) | `4.11` (**0.25x**) | `4.12` (**0.25x**) | `4.31` (**0.26x**) | `4.41` (**0.27x**) | `4.38` (**0.26x**) | `4.09` (**0.25x**) | `4.30` (**0.26x**) | `4.28` (**0.26x**) |
| `W06_join_group_order` | `28.22` | `10.90` (**0.39x**) | `10.74` (**0.38x**) | `10.61` (**0.38x**) | `10.63` (**0.38x**) | `10.63` (**0.38x**) | `10.42` (**0.37x**) | `10.66` (**0.38x**) | `10.81` (**0.38x**) | `10.66` (**0.38x**) | `10.48` (**0.37x**) | `10.49` (**0.37x**) | `11.07` (**0.39x**) | `10.88` (**0.39x**) | `10.76` (**0.38x**) |
| `W07_window_functions` | `50.35` | `39.10` (**0.78x**) | `38.97` (**0.77x**) | `39.22` (**0.78x**) | `38.96` (**0.77x**) | `38.54` (**0.77x**) | `38.66` (**0.77x**) | `38.59` (**0.77x**) | `38.80` (**0.77x**) | `39.26` (**0.78x**) | `38.81` (**0.77x**) | `39.14` (**0.78x**) | `38.80` (**0.77x**) | `38.90` (**0.77x**) | `38.85` (**0.77x**) |
| `W08_indexed_update_delete` | `8.05` | `17.56` (**2.18x**) | `16.46` (**2.04x**) | `15.91` (**1.98x**) | `9.48` (**1.18x**) | `9.61` (**1.19x**) | `9.55` (**1.19x**) | `9.40` (**1.17x**) | `9.47` (**1.18x**) | `9.44` (**1.17x**) | `9.44` (**1.17x**) | `9.52` (**1.18x**) | `9.47` (**1.18x**) | `9.49` (**1.18x**) | `9.53` (**1.18x**) |
| `W09_like_text_scan` | `19.06` | `12.12` (**0.64x**) | `12.02` (**0.63x**) | `11.96` (**0.63x**) | `11.71` (**0.61x**) | `11.78` (**0.62x**) | `11.93` (**0.63x**) | `12.14` (**0.64x**) | `11.97` (**0.63x**) | `11.93` (**0.63x**) | `11.97` (**0.63x**) | `11.93` (**0.63x**) | `12.05` (**0.63x**) | `11.86` (**0.62x**) | `11.85` (**0.62x**) |
| `W10_jsonb_extract_agg` | `10.94` | `30.77` (**2.81x**) | `30.99` (**2.83x**) | `11.69` (**1.07x**) | `11.23` (**1.03x**) | `11.03` (**1.01x**) | `10.99` (**1.00x**) | `11.15` (**1.02x**) | `11.19` (**1.02x**) | `11.14` (**1.02x**) | `11.11` (**1.02x**) | `11.24` (**1.03x**) | `11.12` (**1.02x**) | `11.15` (**1.02x**) | `11.29` (**1.03x**) |
| `W11_fts5_index_and_bm25` | `57.92` | `47.84` (**0.83x**) | `48.77` (**0.84x**) | `47.20` (**0.81x**) | `47.53` (**0.82x**) | `48.40` (**0.84x**) | `46.53` (**0.80x**) | `46.01` (**0.79x**) | `45.36` (**0.78x**) | `45.48` (**0.79x**) | `46.12` (**0.80x**) | `47.20` (**0.81x**) | `46.53` (**0.80x**) | `46.46` (**0.80x**) | `45.55` (**0.79x**) |
| `W12_rtree_insert_and_bbox` | `24.40` | `40.39` (**1.66x**) | `40.46` (**1.66x**) | `40.49` (**1.66x**) | `39.83` (**1.63x**) | `39.92` (**1.64x**) | `23.53` (**0.96x**) | `23.72` (**0.97x**) | `23.68` (**0.97x**) | `23.26` (**0.95x**) | `23.73` (**0.97x**) | `23.54` (**0.96x**) | `24.12` (**0.99x**) | `24.07` (**0.99x**) | `23.50` (**0.96x**) |
| `W13_autocommit_wal_txns` | `4.13` | `7.27` (**1.76x**) | `4.96` (**1.20x**) | `4.93` (**1.19x**) | `4.92` (**1.19x**) | `4.85` (**1.17x**) | `4.83` (**1.17x**) | `4.81` (**1.16x**) | `4.85` (**1.17x**) | `4.88` (**1.18x**) | `4.85` (**1.18x**) | `4.92` (**1.19x**) | `4.92` (**1.19x**) | `5.03` (**1.22x**) | `4.89` (**1.18x**) |
| `W14_cli_sql_script` | `26.79` | `37.24` (**1.39x**) | `36.70` (**1.37x**) | `33.98` (**1.27x**) | `33.79` (**1.26x**) | `33.36` (**1.25x**) | `33.50` (**1.25x**) | `35.16` (**1.31x**) | `35.28` (**1.32x**) | `35.45` (**1.32x**) | `35.59` (**1.33x**) | `35.25` (**1.32x**) | `36.02` (**1.34x**) | `35.85` (**1.34x**) | `36.72` (**1.37x**) |
| **Toplam** | **`295.63`** | **`328.91`** (**1.11x**) | **`314.98`** (**1.06x**) | **`290.03`** (**0.98x**) | **`281.62`** (**0.95x**) | **`263.31`** (**0.89x**) | **`245.12`** (**0.83x**) | **`246.74`** (**0.83x**) | **`246.75`** (**0.83x**) | **`246.27`** (**0.83x**) | **`247.16`** (**0.84x**) | **`248.30`** (**0.84x**) | **`249.18`** (**0.84x**) | **`249.04`** (**0.84x**) | **`248.48`** (**0.84x**) |

---

## 00_performans_taban — `performans@4ccdc64`

**Baz.** `performans` dalının (Saf Rust tavanı, P1-C) bu makinedeki ölçümü; `performans-A` adımlarının karşılaştırma noktası.

- **Testler**: `cargo test --workspace`: 517 geçti, 4 başarısız, 337 yok sayıldı. Başarısız 4 test (`tests/phase7_vtab_framework_and_pragma_tables.rs` içindeki `*_differential_parity`) referans ikili için başka bir makinenin yolunu sabit kodluyor; bu makinede tüm aşamalarda aynı şekilde düşüyor. (C uyumluluk testleri için `sqlite3` 3.54.1 referans ikilisi `PATH`e eklenerek koşuldu.)
- **Toplam**: `328.91 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`1.184x`** (disk WAL `1.097x`, bellek-içi `1.567x`)
- **Tepe RSS**: `17,900 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.73`** | `7.70` | `1.000x` | `9.63` | **`0.80x`** | `8,648` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.34`** | `9.28` | `1.000x` | `8.74` | **`1.07x`** | `9,552` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`37.02`** | `36.42` | `1.000x` | `23.35` | **`1.58x`** | `9,552` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`14.35`** | `14.30` | `1.000x` | `7.47` | **`1.92x`** | `9,616` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.28`** | `17.20` | `1.000x` | `16.59` | **`1.04x`** | `9,616` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.90`** | `10.83` | `1.000x` | `28.22` | **`0.39x`** | `9,684` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`39.10`** | `38.86` | `1.000x` | `50.35` | **`0.78x`** | `12,132` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`17.56`** | `17.48` | `1.000x` | `8.05` | **`2.18x`** | `12,132` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.12`** | `11.93` | `1.000x` | `19.06` | **`0.64x`** | `12,132` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.77`** | `30.54` | `1.000x` | `10.94` | **`2.81x`** | `13,104` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.84`** | `46.97` | `1.000x` | `57.92` | **`0.83x`** | `17,836` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.39`** | `39.70` | `1.000x` | `24.40` | **`1.66x`** | `17,876` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`7.27`** | `6.91` | `1.000x` | `4.13` | **`1.76x`** | `17,908` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`37.24`** | `37.03` | `1.000x` | `26.79` | **`1.39x`** | `17,876` | `fc7490e2b80c68e2` ✓ |

---

## 01_a1_wal_islem — `performans-A@693ac8b`

**Adım 1 — WAL okuma/yazma işlemi açılış-kapanış maliyeti.** Sistem çağrısı sayısı zaten C ile aynıydı; fark kullanıcı alanındaydı (okuma işlemi başına ~32 mutex turu, tek tek doğrulanan ~70 shm kelime okuması). `validate_offset` satır-içi tek koşul oldu (ayrıntılı hata yolu soğuk fonksiyonda aynen duruyor); bağlantı başına bölge-0 önbelleği, kilitsiz kapalı/zehirli bayrağı, birleşik kilit-sonrası doğrulama (aynı kontroller, aynı sıra); bayt sırasına göre kelime döngülü WAL sağlama toplamı; ardışık WAL çerçeveleri tek konumlu yazmada (≤ 64 KiB) toplanıyor. Hedef: `W03`, `W13`, `W04`.

- **Testler**: `cargo test --workspace`: 517 geçti, bilinen 4 başarısız, 337 yok sayıldı. Windows arka ucu derlenmedi (koda dokunulmadı; bölge-0 önbelleğinin dayandığı değişmez orada yalnızca kod okunarak kontrol edildi).
- **Toplam**: `314.98 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`1.110x`** (disk WAL `1.008x`, bellek-içi `1.582x`)
- **Tepe RSS**: `17,904 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.45`** | `7.37` | `7.73` | **`0.964x`** | `0.964x` | `9.63` | **`0.77x`** | `8,508` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`9.11`** | `9.00` | `9.34` | **`0.975x`** | `0.975x` | `8.74` | **`1.04x`** | `9,340` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.88`** | `27.71` | `37.02` | **`0.753x`** | `0.753x` | `23.35` | **`1.19x`** | `9,340` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.57`** | `12.40` | `14.35` | **`0.877x`** | `0.877x` | `7.47` | **`1.68x`** | `9,404` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.89`** | `17.72` | `17.28` | **`1.036x`** | `1.036x` | `16.59` | **`1.08x`** | `9,404` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.74`** | `10.71` | `10.90` | **`0.985x`** | `0.985x` | `28.22` | **`0.38x`** | `9,468` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.97`** | `38.78` | `39.10` | **`0.997x`** | `0.997x` | `50.35` | **`0.77x`** | `11,892` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`16.46`** | `16.35` | `17.56` | **`0.937x`** | `0.937x` | `8.05` | **`2.04x`** | `11,892` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.02`** | `11.78` | `12.12` | **`0.992x`** | `0.992x` | `19.06` | **`0.63x`** | `11,892` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`30.99`** | `30.78` | `30.77` | **`1.007x`** | `1.007x` | `10.94` | **`2.83x`** | `12,928` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`48.77`** | `47.33` | `47.84` | **`1.019x`** | `1.019x` | `57.92` | **`0.84x`** | `17,752` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.46`** | `39.87` | `40.39` | **`1.002x`** | `1.002x` | `24.40` | **`1.66x`** | `15,752` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.96`** | `4.77` | `7.27` | **`0.682x`** | `0.682x` | `4.13` | **`1.20x`** | `15,784` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.70`** | `36.21` | `37.24` | **`0.986x`** | `0.986x` | `26.79` | **`1.37x`** | `15,752` | `fc7490e2b80c68e2` ✓ |

---

## 02_a2_jsonb_indeks — `performans-A@e2f7843`

**Adım 2 — ham indeks yaprağı yeniden dengeleme ve indeks sıralı akışlı kümeleme.** `W10`daki fark JSON'dan değil iki genel yoldan geliyordu: (1) dolu indeks yaprağına her eklemede üst düğüm + 3 kardeş yaprak hücre hücre `Vec`e ayrıştırılıp yeniden yazılıyordu; `try_insert_index_balance_raw` aynı dağıtımı ham sayfa baytları üzerinde yapıyor (sayfa çıktısı eski yolla bayt bayt aynı, desteklemediği her durumda sayfaya dokunmadan eski yola düşüyor). (2) Akışlı kümeleme planları kapsayıcı olmayan indeks taramasını reddediyordu; yeni `IndexOrderedTableLookup` erişim yolu eklendi. Ayrıca kayıt karşılaştırmada tamsayı-tamsayı hızlı yolu. **Davranış düzeltmesi:** `HashGroupByPlan` tamsayı `1` ile gerçel `1.0` anahtarlarını iki ayrı grup sayıyordu; artık SQLite'taki gibi tek grup. Hedef: `W10`; yan kazanç `W02`, `W08`, `W14`.

- **Testler**: `cargo test --workspace`: 520 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. 9.000 satırlık rastgele indeksli yükte yazılan veritabanı baz ikilininkiyle bayt bayt aynı ve C `integrity_check` geçiyor. Sayfa başına ayrılmış bayt (reserved bytes) kullanan veritabanlarında denenmedi.
- **Toplam**: `290.03 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`1.018x`** (disk WAL `0.989x`, bellek-içi `1.131x`)
- **Tepe RSS**: `17,696 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.43`** | `7.37` | `7.45` | **`0.997x`** | `0.961x` | `9.63` | **`0.77x`** | `8,636` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`8.57`** | `8.43` | `9.11` | **`0.941x`** | `0.918x` | `8.74` | **`0.98x`** | `9,468` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.80`** | `27.48` | `27.88` | **`0.997x`** | `0.751x` | `23.35` | **`1.19x`** | `9,468` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.71`** | `12.61` | `12.57` | **`1.011x`** | `0.886x` | `7.47` | **`1.70x`** | `9,532` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`17.52`** | `17.49` | `17.89` | **`0.979x`** | `1.014x` | `16.59` | **`1.06x`** | `9,532` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.61`** | `10.55` | `10.74` | **`0.988x`** | `0.974x` | `28.22` | **`0.38x`** | `9,596` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`39.22`** | `38.44` | `38.97` | **`1.006x`** | `1.003x` | `50.35` | **`0.78x`** | `11,940` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`15.91`** | `15.73` | `16.46` | **`0.966x`** | `0.906x` | `8.05` | **`1.98x`** | `11,940` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.96`** | `11.85` | `12.02` | **`0.996x`** | `0.987x` | `19.06` | **`0.63x`** | `11,940` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.69`** | `11.50` | `30.99` | **`0.377x`** | `0.380x` | `10.94` | **`1.07x`** | `12,124` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.20`** | `46.03` | `48.77` | **`0.968x`** | `0.987x` | `57.92` | **`0.81x`** | `17,484` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`40.49`** | `39.45` | `40.46` | **`1.001x`** | `1.003x` | `24.40` | **`1.66x`** | `17,552` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.93`** | `4.66` | `4.96` | **`0.996x`** | `0.679x` | `4.13` | **`1.19x`** | `17,584` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.98`** | `33.63` | `36.70` | **`0.926x`** | `0.912x` | `26.79` | **`1.27x`** | `17,552` | `fc7490e2b80c68e2` ✓ |

---

## 03_a3_nokta_dml — `performans-A@946f61e`

**Adım 3 — satır kimliğiyle `UPDATE`/`DELETE` için özel planlar.** Hazırlanmış `UPDATE … WHERE rowid = ?` ve `DELETE … WHERE rowid = ?` genel VDBE yorumlayıcısından geçiyor, satır başına ikinci bir B-tree inişi ve tam kayıt çöz/yeniden kodla yapıyordu. `RowidPointUpdatePlan` / `RowidPointDeletePlan` tek ham arama yapıp değişmeyen alanları ham bayt olarak kopyalıyor; aynı indeks sil/ekle ve satır değiştir çağrılarını aynı baytlarla yapıyor. Yalnızca düz `main` tabloları için (tetikleyici, yabancı anahtar, `CHECK`, `STRICT`, üretilmiş sütun, kısmi/ifade indeksi, özel harmanlama, çakışma yan tümcesi, `RETURNING` yok); diğer her durumda VDBE programı çalışıyor. Ek olarak konumlu tablo işlemleri, ham yaprak yeniden paketleme ve bir kez çözülen indeks arama anahtarı (`RawRecordKey`). Hedef: `W08`; yan kazanç `W02`.

- **Testler**: `cargo test --workspace`: 524 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Rastgele farksal koşu (1.900 tohum, ~2,1 milyon ifade): çıktı, hata kodları, değişiklik sayaçları ve veritabanı baytları adım 2 ile aynı; tüm veritabanları C `integrity_check` geçiyor. Plan ortasında gelen G/Ç hatası / meşgul / kesme durumları hata enjeksiyonuyla denenmedi.
- **Toplam**: `281.62 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.969x`** (disk WAL `0.933x`, bellek-içi `1.112x`)
- **Tepe RSS**: `17,652 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.48`** | `7.45` | `7.43` | **`1.007x`** | `0.968x` | `9.63` | **`0.78x`** | `8,588` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.55`** | `7.44` | `8.57` | **`0.881x`** | `0.808x` | `8.74` | **`0.86x`** | `9,428` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.71`** | `27.58` | `27.80` | **`0.997x`** | `0.749x` | `23.35` | **`1.19x`** | `9,428` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`12.70`** | `12.58` | `12.71` | **`0.999x`** | `0.885x` | `7.47` | **`1.70x`** | `9,492` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`18.09`** | `17.98` | `17.52` | **`1.033x`** | `1.047x` | `16.59` | **`1.09x`** | `9,556` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.59` | `10.61` | **`1.002x`** | `0.975x` | `28.22` | **`0.38x`** | `9,620` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.96`** | `38.61` | `39.22` | **`0.994x`** | `0.996x` | `50.35` | **`0.77x`** | `11,800` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.48`** | `9.36` | `15.91` | **`0.596x`** | `0.540x` | `8.05` | **`1.18x`** | `11,800` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.71`** | `11.66` | `11.96` | **`0.978x`** | `0.966x` | `19.06` | **`0.61x`** | `11,800` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.23`** | `11.16` | `11.69` | **`0.961x`** | `0.365x` | `10.94` | **`1.03x`** | `11,984` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.53`** | `46.18` | `47.20` | **`1.007x`** | `0.994x` | `57.92` | **`0.82x`** | `17,364` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`39.83`** | `39.20` | `40.49` | **`0.984x`** | `0.986x` | `24.40` | **`1.63x`** | `17,440` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.92`** | `4.66` | `4.93` | **`0.996x`** | `0.677x` | `4.13` | **`1.19x`** | `17,472` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.79`** | `33.62` | `33.98` | **`0.994x`** | `0.907x` | `26.79` | **`1.26x`** | `17,440` | `fc7490e2b80c68e2` ✓ |

---

## 04_a4_kapsayan_indeks — `performans-A@40e275d`

**Adım 4 — kapsayan indeks taramalarında tamsayı çekirdeği.** `W04` ve `W05` zamanlarını hücre başına genel işte harcıyordu: ziyaret edilen her hücre için tam kayıt-aralık karşılaştırması, satır başına genel başlık ayrıştırma ve enum dağıtımlı `WHERE`/kümeleme değerlendirmesi. `IntRangeProbe` aralık hedefini arama başına bir kez çözüyor (tüm işlenenler `INTEGER` ve `DESC` sütun yoksa); `RawIndexLeafView` sayfa başına değişmezleri döngü dışına alıyor; `CovIntKernelShape` (`fast_read/int_kernel.rs`), `WHERE` sütun karşılaştırmaları/`BETWEEN` ve kümeler `count/sum/total/avg/min/max` olduğunda satırları tamsayı aralık testi + mevcut `FastSumAccum` ile işliyor (taşma anlambilimi aynı). Çekirdeğin çözemediği her satır ve tamsayı olmayan parametreli her yürütme mevcut genel koddan geçiyor. Hedef: `W04`, `W05`.

- **Testler**: `cargo test --workspace`: 527 geçti (3 yeni), bilinen 4 başarısız, 337 yok sayıldı. Farksal koşular: 2.000 tohum (400 bin sorgu, yarısı bozuk indeks sayfalarıyla) ve 700 rastgele aralık/kümeleme sorgusu adım 3 ile birebir aynı; ikincisi ondalık biçimi normalleştirilince C ile de aynı. `W11` (+%3) ve `W09` (+%2,5) aynı komut sayısıyla yavaş okunuyor (kod yerleşimi; bu iş yüklerinin koduna dokunulmadı).
- **Toplam**: `263.31 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.846x`** (disk WAL `0.785x`, bellek-içi `1.113x`)
- **Tepe RSS**: `18,228 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.35`** | `7.29` | `7.48` | **`0.982x`** | `0.950x` | `9.63` | **`0.76x`** | `8,720` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.56`** | `7.50` | `7.55` | **`1.002x`** | `0.810x` | `8.74` | **`0.86x`** | `9,560` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.97`** | `27.67` | `27.71` | **`1.009x`** | `0.756x` | `23.35` | **`1.20x`** | `9,560` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.91`** | `7.73` | `12.70` | **`0.622x`** | `0.551x` | `7.47` | **`1.06x`** | `9,624` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.41`** | `4.38` | `18.09` | **`0.244x`** | `0.255x` | `16.59` | **`0.27x`** | `9,624` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.58` | `10.63` | **`1.000x`** | `0.975x` | `28.22` | **`0.38x`** | `9,816` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.54`** | `38.24` | `38.96` | **`0.989x`** | `0.986x` | `50.35` | **`0.77x`** | `13,272` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.61`** | `9.48` | `9.48` | **`1.013x`** | `0.547x` | `8.05` | **`1.19x`** | `13,272` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.78`** | `11.73` | `11.71` | **`1.006x`** | `0.972x` | `19.06` | **`0.62x`** | `13,272` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.03`** | `10.91` | `11.23` | **`0.982x`** | `0.358x` | `10.94` | **`1.01x`** | `13,240` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`48.40`** | `46.38` | `47.53` | **`1.018x`** | `1.012x` | `57.92` | **`0.84x`** | `18,016` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`39.92`** | `39.51` | `39.83` | **`1.002x`** | `0.988x` | `24.40` | **`1.64x`** | `18,032` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.85`** | `4.57` | `4.92` | **`0.987x`** | `0.668x` | `4.13` | **`1.17x`** | `18,064` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.36`** | `33.22` | `33.79` | **`0.987x`** | `0.896x` | `26.79` | **`1.25x`** | `18,032` | `fc7490e2b80c68e2` ✓ |

---

## 05_a5_rtree — `performans-A@700ded4`

**Adım 5 — R-Tree yerinde düğüm güncelleme ve sanal tablo işlem sırasının belirlenimci yapılması.** `W12`nin süresi disk G/Ç'si değil CPU işiydi: bölünmeyen eklemelerde ve üst düğüm sınır kutusu düzeltmesinde düğüm `klonla → değiştir → geri koy` yerine yerinde değiştiriliyor, tam düğüm anlık görüntüsü yerine küçük geri alma kayıtları tutuluyor; düğüm sınır kutusu önbelleği, tek sorgulu önbellek erişimi, düğüm numaralı haritalar için ucuz hasher; `SELECT`in `WHERE` ifadesi yürütme başına 4 kez derin kopyalanıyordu, artık ödünç alınıyor. Gölge tablo baytları değişmedi. **Belirlenimsizlik düzeltmesi (tabanda da vardı, doğrulama sırasında bulundu):** aynı işlemde iki veya daha çok sanal tablo değişince dosya, süreç başına rastgele seçilen birkaç bayt-farklı varyanttan biri oluyordu (işlem yayılımı rastgele tohumlu `HashMap` sırasıyla yürüyordu). Artık tablolar işleme katılma sırasıyla işleniyor (C'deki `db->aVTrans` sırası). Mantıksal içerik hiç etkilenmiyordu; fark yalnızca gölge tablolar arasındaki sayfa tahsis sırasındaydı. Hedef: `W12`; yan kazanç `W11`.

- **Testler**: `cargo test --workspace`: 533 geçti (6 yeni), bilinen 4 başarısız, 337 yok sayıldı. Çok sanal tablolu iki tekrar betiği 16'şar koşuda tek dosya üretiyor (önceden iki); çıktı adım 4 ile aynı, C `rtreecheck` + `integrity_check` `ok`. Farksal koşu: 650 tek R-Tree tohumu çıktı ve dosya baytlarında adım 4 ile aynı; 300 çok R-Tree tohumu çıktıda aynı ve koşudan koşuya kendisiyle aynı. Dokunulmayan bilinen hatalar (tabanda var): 512 bayt sayfalı bazı R-Tree dosyalarında C `integrity_check` → `Child page depth differs`; R-Tree satır kimliği tahsisi `i64::MAX`ta taşıyor. Ayrıntı: `saglik/c_uyumluluk_ve_saglik_analizi.md`.
- **Toplam**: `245.12 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.811x`** (disk WAL `0.784x`, bellek-içi `0.920x`)
- **Tepe RSS**: `17,892 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.47`** | `7.43` | `7.35` | **`1.017x`** | `0.966x` | `9.63` | **`0.78x`** | `8,840` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.68`** | `7.59` | `7.56` | **`1.015x`** | `0.822x` | `8.74` | **`0.88x`** | `9,680` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.86`** | `27.53` | `27.97` | **`0.996x`** | `0.753x` | `23.35` | **`1.19x`** | `9,680` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.84`** | `7.75` | `7.91` | **`0.992x`** | `0.547x` | `7.47` | **`1.05x`** | `9,744` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.33`** | `4.31` | `4.41` | **`0.981x`** | `0.250x` | `16.59` | **`0.26x`** | `9,744` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.42`** | `10.36` | `10.63` | **`0.980x`** | `0.956x` | `28.22` | **`0.37x`** | `9,936` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.66`** | `38.40` | `38.54` | **`1.003x`** | `0.989x` | `50.35` | **`0.77x`** | `12,416` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.55`** | `9.44` | `9.61` | **`0.994x`** | `0.544x` | `8.05` | **`1.19x`** | `12,416` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.93`** | `11.77` | `11.78` | **`1.013x`** | `0.984x` | `19.06` | **`0.63x`** | `12,416` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`10.99`** | `10.97` | `11.03` | **`0.997x`** | `0.357x` | `10.94` | **`1.00x`** | `12,664` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.53`** | `44.99` | `48.40` | **`0.961x`** | `0.973x` | `57.92` | **`0.80x`** | `17,668` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.53`** | `22.79` | `39.92` | **`0.590x`** | `0.583x` | `24.40` | **`0.96x`** | `17,764` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.83`** | `4.49` | `4.85` | **`0.994x`** | `0.664x` | `4.13` | **`1.17x`** | `15,672` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`33.50`** | `33.27` | `33.36` | **`1.004x`** | `0.900x` | `26.79` | **`1.25x`** | `15,640` | `fc7490e2b80c68e2` ✓ |

---

## 05b_duzeltme_dengesiz_agac — `performans-A@1c12e74`

**Adım 5b — doğruluk düzeltmesi (performans amacı yok).** Üç hata da ilk port commit'inden (`814e41e`) beri vardı. (1) **Dengesiz tablo B-tree'si:** silme, kök olmayan bir iç sayfayı tek çocukla bırakınca o sayfa serbest bırakılıp üst düğüm doğrudan toruna bağlanıyordu; alt ağaç kardeşlerinden bir seviye kısa kalıyordu. C bunu bozulma sayıyor (`Child page depth differs`), motorun kendisi de sonradan aynı ağaçta hata verip satır kaybedebiliyordu. Artık sayfa korunup aynı seviyedeki kardeşleriyle C'deki gibi yeniden dengeleniyor; derinlik yalnızca kökte değişiyor. (2) **İndeks sayfasının hücre sayısına göre bölünmesi:** çok farklı boyutlu anahtarlarda bir yarı sayfaya sığmıyor ve `INSERT` `cells exceed usable B-tree page capacity` ile düşüyordu (C'de başarılı); bölünme noktası artık iki yarının da sığdığı en yakın konum (önceden çalışan bölünmeler bayt bayt aynı). (3) **`PRAGMA integrity_check`:** C'nin `btree.c` kontrolleri C'nin mesaj metinleriyle port edildi — B-tree düzeyinde 30 kontrolün 29'u (önce 6), SQL düzeyinde 24'ün 18'i (önce 2) C ile aynı. Ayrıntı: `saglik/integrity_check_parity.md`.

- **Testler**: `cargo test --workspace`: 542 geçti (9 yeni), bilinen 4 başarısız, 337 yok sayıldı. Üç tekrar betiği artık C `integrity_check` + `rtreecheck` geçiyor ve ağaç başına tek yaprak derinliği var; yeni Rust kontrolü düzeltme öncesi yazılmış bozuk dosyalarda C ile aynı mesajı veriyor. Hataya hiç girmeyen 13 senaryonun dosyaları önceki commit ile bayt bayt aynı. Farksal koşu: 12.960 + 1.920 çalıştırma, her dosya C `integrity_check` geçiyor. **Hız:** `W01`–`W13` komut sayıları değişmedi (hiçbir iş yükü düzeltilen yollara girmiyor; önceki kazançların hiçbiri bu hatalara dayanmıyordu). `W14` +%5: betik `PRAGMA integrity_check` ile bitiyor ve kontrol artık C'nin yaptığı işi yapıyor (eski 2,0 M komut, C 5,4 M, yeni 14,4 M). Doğrulanmayan: commit bazında ikiye bölme; `auto_vacuum` + iç sayfa yeniden dengeleme için ayrı C farksalı; kasıtlı bozulmuş 6.000 dosyanın 5'inde C hata verirken motor vermiyor (sayfa 1 hasarını şema yükleyici kabul ediyor).
- **Toplam**: `246.74 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.813x`** (disk WAL `0.785x`, bellek-içi `0.923x`)
- **Tepe RSS**: `17,924 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.57`** | `7.45` | `7.47` | **`1.014x`** | `0.979x` | `9.63` | **`0.79x`** | `8,764` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.47`** | `7.42` | `7.68` | **`0.973x`** | `0.800x` | `8.74` | **`0.85x`** | `9,604` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.02`** | `27.49` | `27.86` | **`1.006x`** | `0.757x` | `23.35` | **`1.20x`** | `9,604` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.92`** | `7.83` | `7.84` | **`1.010x`** | `0.552x` | `7.47` | **`1.06x`** | `9,668` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.11`** | `4.09` | `4.33` | **`0.950x`** | `0.238x` | `16.59` | **`0.25x`** | `9,668` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.66`** | `10.62` | `10.42` | **`1.023x`** | `0.978x` | `28.22` | **`0.38x`** | `9,860` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.59`** | `38.41` | `38.66` | **`0.998x`** | `0.987x` | `50.35` | **`0.77x`** | `12,340` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.40`** | `9.34` | `9.55` | **`0.985x`** | `0.535x` | `8.05` | **`1.17x`** | `12,340` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.14`** | `11.98` | `11.93` | **`1.018x`** | `1.001x` | `19.06` | **`0.64x`** | `12,340` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.15`** | `11.03` | `10.99` | **`1.014x`** | `0.363x` | `10.94` | **`1.02x`** | `12,524` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.01`** | `45.16` | `46.53` | **`0.989x`** | `0.962x` | `57.92` | **`0.79x`** | `17,700` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.72`** | `23.09` | `23.53` | **`1.008x`** | `0.587x` | `24.40` | **`0.97x`** | `17,788` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.81`** | `4.47` | `4.83` | **`0.997x`** | `0.662x` | `4.13` | **`1.16x`** | `17,820` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.16`** | `34.87` | `33.50` | **`1.050x`** | `0.944x` | `26.79` | **`1.31x`** | `17,788` | `fc7490e2b80c68e2` ✓ |

---

## 05d_duzeltme_wal_cokme — `performans-A@3707ece`

**Adım 5d — doğruluk düzeltmesi: WAL çökme atomikliği (performans amacı yok).** 5c envanterinde bulundu; WAL yazıcısı port edildiğinden beri vardı. Büyük bir yazma işleminde sayfa önbelleği taşıp sayfalar commit'ten önce `-wal`e yazıldığında ve taşmış bir sayfa yeniden değiştiğinde, `COMMIT` eski çerçeveyi yerinde yeniden yazar ve sonuna (commit çerçevesi dahil) yeni çerçeveler ekler. C bu sırada eklenen çerçeveleri tuz ve sağlama toplamı yerine 16 sıfır baytla yazar (`walEncodeFrame`, `iReCksum != 0`); geçerli zinciri ancak `walRewriteChecksums` kurar. Motor bu çerçeveleri gerçek tuz ve eski görüntüler üzerinden zincirlenmiş sağlama toplamıyla yazıyordu: çökme, eklenen çerçeveleri koruyup yerinde yeniden yazımı kaybederse **yarım uygulanmış bir işlem** görünür oluyordu (C aynı durumda işlemi atar). Artık C ile aynı: bekleyen yeniden yazım varken eklenen çerçeveler doğrulanamaz biçimde yazılıyor. Yazma sırası ve adım 1'deki çerçeve toplama değişmedi.

- **Testler**: `cargo test --workspace`: 546 geçti (4 yeni), bilinen 4 başarısız, 337 yok sayıldı. Dört yeni test, senkronlanmamış `-wal` yazımlarının her alt kümesini (test başına ~1.000–4.100 çökme görüntüsü) dener; kurtarma ya işlem öncesi durumu ya da işlemin tamamını vermeli. Düzeltme geri alınınca dördü de düşüyor. Süreç öldürme + blok geri yükleme betiği: düzeltmeden önce 600 satır ve iki güncellemeden biri uygulanmış; sonra C yazıcısıyla aynı sonuç (işlem atılıyor). **Hız:** toplam 0,999x; ölçüm iş yükleri önbellek taşırmadığı için `W13`/`W01` komut ve sistem çağrısı sayıları aynı. Doğrulanmayan: gerçek güç kesintisi (tüm çökme durumları sentetik). Kod okunarak bulunan ve düzeltilmeyen iki ara-durum farkı daha var (`journal_size_limit` kırpma hatası; `load_deserialized_image` içinde senkronsuz kapanış checkpoint'i).
- **Toplam**: `246.75 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.815x`** (disk WAL `0.788x`, bellek-içi `0.920x`)
- **Tepe RSS**: `17,884 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.53`** | `7.44` | `7.57` | **`0.995x`** | `0.974x` | `9.63` | **`0.78x`** | `8,828` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.63`** | `7.45` | `7.47` | **`1.021x`** | `0.817x` | `8.74` | **`0.87x`** | `9,668` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.11`** | `27.71` | `28.02` | **`1.003x`** | `0.759x` | `23.35` | **`1.20x`** | `9,668` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.95`** | `7.86` | `7.92` | **`1.003x`** | `0.554x` | `7.47` | **`1.06x`** | `9,732` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.12`** | `4.10` | `4.11` | **`1.002x`** | `0.238x` | `16.59` | **`0.25x`** | `9,732` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.81`** | `10.76` | `10.66` | **`1.014x`** | `0.992x` | `28.22` | **`0.38x`** | `9,924` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.80`** | `38.37` | `38.59` | **`1.005x`** | `0.992x` | `50.35` | **`0.77x`** | `12,468` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.47`** | `9.36` | `9.40` | **`1.007x`** | `0.539x` | `8.05` | **`1.18x`** | `12,468` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.97`** | `11.90` | `12.14` | **`0.986x`** | `0.988x` | `19.06` | **`0.63x`** | `12,468` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.19`** | `11.07` | `11.15` | **`1.003x`** | `0.364x` | `10.94` | **`1.02x`** | `12,716` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.36`** | `44.73` | `46.01` | **`0.986x`** | `0.948x` | `57.92` | **`0.78x`** | `17,640` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.68`** | `23.29` | `23.72` | **`0.999x`** | `0.586x` | `24.40` | **`0.97x`** | `17,712` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.85`** | `4.73` | `4.81` | **`1.009x`** | `0.668x` | `4.13` | **`1.17x`** | `17,744` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.28`** | `34.78` | `35.16` | **`1.003x`** | `0.947x` | `26.79` | **`1.32x`** | `17,712` | `fc7490e2b80c68e2` ✓ |

---

## 05e_duzeltme_guvence — `performans-A@e53008f`

**Adım 5e — doğruluk düzeltmeleri: güvence ve uyumluluk hataları (performans amacı yok).** (1) `PRAGMA secure_delete` yok sayılıyordu (satır dönmüyor, silinen hücrelerin içeriği dosyada kalıyordu); tüm biçimleriyle uygulandı, `ON` iken dosyada yalnızca canlı satırların baytları kalıyor. (2) C'de satır dönen ama burada dönmeyen pragmalar: `analysis_limit` (saklanıyor ve `ANALYZE` tarafından uygulanıyor), `data_version`, `cell_size_check` (yüklenen sayfalar C'deki gibi doğrulanıyor), `mmap_size`. (3) `ALTER TABLE ADD COLUMN` sütun metnini AST'den yeniden üretiyordu: `DEFAULT x'00'` → `x''` (varsayılan değer **kayboluyordu**), sabit olmayan varsayılan kabul ediliyordu; artık kullanıcının metni C'deki gibi aynen ekleniyor, C'nin kontrolleri C'nin sırası ve mesajlarıyla çalışıyor. (4) Tablosu olmayan veritabanının başlığı: şema biçimi ve kodlama ilk şema satırına kadar 0 kalıyor ve `PRAGMA encoding` o ana kadar geçerli. (5) İki WAL hata yolu: `journal_size_limit` kırpma hatası artık commit çerçevesi kalıcı olmuş bir `COMMIT`i başarısız yapmıyor; `load_deserialized_image` WAL'i senkronlu checkpoint ile kapatıyor.

- **Testler**: `cargo test --workspace`: 559 geçti (13 yeni), bilinen 4 başarısız, 337 yok sayıldı. `ADD COLUMN` farksalı 1.126 durumda C ile eşit: 1.118 (önce 407). Ayrışma bulucu (192 senaryo): hiçbir senaryo daha kötü katmanda bitmiyor, 11'i iyileşti. 13 düzen senaryosu önceki commit ile bayt bayt aynı. **Hız:** toplam 0,998x; komut sayıları aynı (`W08`/`W09` +%0,05). `W05` +%5 aynı komut sayısıyla (kod yerleşimi). **Bilerek C'den farklı bırakılan (mevcut entegrasyon testleri eski davranışı doğruluyor):** `ADD COLUMN` satır kontrolü hatası `SQLITE_CONSTRAINT` dönüyor (C: `SQLITE_ERROR`); `too many columns on t` (C: `… on sqlite_altertab_t`). Doğrulanmayan: deserialize yolu çalışan bir C ikilisine karşı.
- **Toplam**: `246.27 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.813x`** (disk WAL `0.788x`, bellek-içi `0.914x`)
- **Tepe RSS**: `17,976 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.48`** | `7.38` | `7.53` | **`0.993x`** | `0.967x` | `9.63` | **`0.78x`** | `8,820` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.63`** | `7.55` | `7.63` | **`1.001x`** | `0.817x` | `8.74` | **`0.87x`** | `9,660` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.63`** | `27.41` | `28.11` | **`0.983x`** | `0.746x` | `23.35` | **`1.18x`** | `9,660` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.71`** | `7.67` | `7.95` | **`0.970x`** | `0.538x` | `7.47` | **`1.03x`** | `9,724` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.31`** | `4.29` | `4.12` | **`1.047x`** | `0.249x` | `16.59` | **`0.26x`** | `9,788` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.66`** | `10.61` | `10.81` | **`0.986x`** | `0.978x` | `28.22` | **`0.38x`** | `9,916` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`39.26`** | `38.75` | `38.80` | **`1.012x`** | `1.004x` | `50.35` | **`0.78x`** | `12,396` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.44`** | `9.40` | `9.47` | **`0.996x`** | `0.537x` | `8.05` | **`1.17x`** | `12,396` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.93`** | `11.79` | `11.97` | **`0.997x`** | `0.984x` | `19.06` | **`0.63x`** | `12,396` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.14`** | `11.06` | `11.19` | **`0.996x`** | `0.362x` | `10.94` | **`1.02x`** | `12,584` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.48`** | `44.68` | `45.36` | **`1.003x`** | `0.951x` | `57.92` | **`0.79x`** | `17,692` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.26`** | `22.98` | `23.68` | **`0.982x`** | `0.576x` | `24.40` | **`0.95x`** | `17,752` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.88`** | `4.56` | `4.85` | **`1.006x`** | `0.672x` | `4.13` | **`1.18x`** | `17,784` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.45`** | `35.01` | `35.28` | **`1.005x`** | `0.952x` | `26.79` | **`1.32x`** | `17,752` | `fc7490e2b80c68e2` ✓ |

---

## 05f_duzeltme_fts5 — `performans-A@e4cdaff`

**Adım 5f — doğruluk düzeltmesi: FTS5'in C ile iki yönlü uyumu (performans amacı yok).** Bu değişiklikten önce iki motor bir FTS5 tablosunu güvenle paylaşamıyordu (farksal koşuda 300 tohumun 300'ü en az bir yönde başarısızdı). Düzeltilenler: **silme bayrağı** (okuma: C'nin `UPDATE`inden sonra `MATCH` satır kaçırıyor, ardından `'optimize'` o kayıtları kalıcı siliyordu; yazma: aynı boşaltmada silinip yeniden eklenen satırın bayrağı kayboluyordu); **64 KiB'ı aşan yaprak** (C: bozuk); **`contentless_delete=1`** kayıt biçimi; **ayraçlar** (unicode61, ascii, porter, trigram C'den bayt düzeyinde port edildi — katlama/aksan/kök bulma farklıydı, C `checksum mismatch` veriyordu); **çakışma yan tümceleri** (`INSERT OR REPLACE` vb.); **`contentless_unindexed=1`, `locale=1`**; FTS5 içinde gerçel→metin çevrimi. Ayrıca sorgu dilbilgisi, `bm25`/`rank`, `highlight`, `snippet`, `fts5vocab`, özel komutlar ve hata metinleri C'ye getirildi. **Kapsam dışı (yapısal, ayrı karar):** segment birleştirme takvimi ve boşaltma zamanı, yaprak boyutu hedefi ve doclist indeksi, yapı çerezi.

- **Testler**: `cargo test --workspace`: 597 geçti (38 yeni; 34'ü C CLI'sinden üretilmiş, gömülü C gölge tablo görüntüleriyle), bilinen 4 başarısız, 337 yok sayıldı. Yeni 34 durumun 32'si önceki kaynakta düşüyor. Farksal koşu (rastgele yapılandırma, işlemler ve sorgular; Rust yazar / C yazar / sırayla yazar; dört motor-dosya birleşiminde bütünlük kontrolü): 3.600 tohum / 10.800 çalıştırma, 0 hata (önce 900 çalıştırmanın 863'ü). 60 tohum yalnızca `secure-delete=1` ile farklı; her biri C'nin kendi indeksini bozmasına dayanıyor (51'i yalnız C'nin yazdığı geçmişte). **Hız:** toplam 1,004x; `W11` +%2,6 (komut sayısı 248,20 M → 247,86 M; `bm25`in C ile birebir aynı olması için eşleşen satır başına daha çok bellek). Düzeltilmeyen: `fts5_locale()` yok; `LIMIT` ötesindeki satırlar için yardımcı fonksiyon değerlendirmesi; `%_config` sürümü bağlanırken denetlenmiyor.
- **Toplam**: `247.16 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.816x`** (disk WAL `0.789x`, bellek-içi `0.923x`)
- **Tepe RSS**: `18,152 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.53`** | `7.42` | `7.48` | **`1.007x`** | `0.974x` | `9.63` | **`0.78x`** | `8,976` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.66`** | `7.59` | `7.63` | **`1.003x`** | `0.820x` | `8.74` | **`0.88x`** | `9,816` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.66`** | `27.46` | `27.63` | **`1.001x`** | `0.747x` | `23.35` | **`1.18x`** | `9,816` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.82`** | `7.75` | `7.71` | **`1.013x`** | `0.545x` | `7.47` | **`1.05x`** | `9,880` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.41`** | `4.39` | `4.31` | **`1.022x`** | `0.255x` | `16.59` | **`0.27x`** | `9,880` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.48`** | `10.40` | `10.66` | **`0.983x`** | `0.961x` | `28.22` | **`0.37x`** | `10,008` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.81`** | `38.64` | `39.26` | **`0.989x`** | `0.993x` | `50.35` | **`0.77x`** | `12,552` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.44`** | `9.38` | `9.44` | **`1.000x`** | `0.537x` | `8.05` | **`1.17x`** | `12,552` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.97`** | `11.70` | `11.93` | **`1.003x`** | `0.987x` | `19.06` | **`0.63x`** | `12,552` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.11`** | `11.05` | `11.14` | **`0.997x`** | `0.361x` | `10.94` | **`1.02x`** | `12,736` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.12`** | `45.70` | `45.48` | **`1.014x`** | `0.964x` | `57.92` | **`0.80x`** | `18,000` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.73`** | `23.39` | `23.26` | **`1.021x`** | `0.588x` | `24.40` | **`0.97x`** | `18,048` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.85`** | `4.68` | `4.88` | **`0.994x`** | `0.668x` | `4.13` | **`1.18x`** | `18,080` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.59`** | `35.36` | `35.45` | **`1.004x`** | `0.956x` | `26.79` | **`1.33x`** | `18,048` | `fc7490e2b80c68e2` ✓ |

---

## 05g_hizalama_dosya — `performans-A@fc4f0ca`

**Adım 5g — C'ye hizalama: dosya düzeyi, bedelsiz sınıflar.** Aynı ifade dizisi için yazılan dosyayı C'nin yazdığına yaklaştırır. (1) **Değişim sayacı** 0'dan başlıyor ve WAL commit'inde C gibi sabit kalıyor. (2) **Şema çerezi** satır başına değil ifade başına artıyor; `PRAGMA schema_version` her ifadeden sonra C ile aynı. (3) **Satır sıraları**: sanal tablo ve gölge tablolarının `sqlite_schema` sırası, `ANALYZE`in `sqlite_stat1` sırası, `CREATE TABLE … UNIQUE` yer tutucu satır dizisi, `VACUUM` ve `DROP TABLE` sırası. (4) **`REAL` sütunda tam değerli sayı** C'nin kuralıyla tamsayı olarak saklanıyor (dosya 1,8 kat küçülüyor); okuma tarafı da düzeltildi (C'nin yazdığı dosyada bu değerler bazı yollarda `integer` dönüyordu). `jsonb_set` vb. metni C'nin eleman türüyle yazıyor. (5) **Serbest bırakılan sayfalar** artık günlüğe alınmıyor, sıfırlanmıyor ve yeniden yazılmıyor (`DROP TABLE` 6.260 → 13 yazma çağrısı; C 25); `secure_delete` `ON`/`FAST`/`OFF` sayfa kararları C ile aynı. (6) **Serbest liste sırası** (ilk gövdenin ilk yaprağı / en yakın yaprak) ve **satır başına yazma sırası** (önce indeksler, sonra tablo satırı; `UPDATE`te yalnızca C'nin yeniden yazdığı indeksler). (7) Günlük başlığı 512 bayt; kısmi checkpoint C'nin sayfa seçimi; yeni dosyalar 0644 ve yan dosyalar veritabanının iznini alıyor. İki ek bulgu düzeltildi: iki `UNIQUE` ihlalinde hata sırası C'ninki oldu.

- **Testler**: `cargo test --workspace`: 626 geçti (29 yeni; 19'u C'nin yazdığı dosyayla bayt eşitliği), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu (192 senaryo):** hiçbir senaryo daha kötü katmanda bitmiyor; **22'si artık C ile bayt bayt aynı (önce 0)**, 42'si yalnızca kullanılmayan baytlarda farklı. **Karar eşliği** (izli C derlemesi ile Rust karar kayıtlarının olay olay karşılaştırması, 234 betik): çerez 234/234, `REAL` saklama 234/234, JSONB 234/234, şema satırları 227/234, serbest bırakma 181/234, tahsis 191/234, yazma sırası 170/234 — son üçü ağaç şekline bağlı; uyuşmayan betikler kapsam dışı sınıfların (silmede yeniden dengeleme, indeks doluluğu, bölünmeler, FTS5/R-Tree yazıcıları) ifadelerinde ayrışıyor. Ayrıntı: `saglik/karar_esligi_5g.md`. **Çökme güvenliği:** 258 geri alma betiği ve commit sırasında öldürülen 2.592 kurtarma (iki yazıcı × iki kurtarıcı), 0 hata (tmpfs; gerçek güç kesintisi yok). **Hız:** toplam 0,998x; en büyük komut artışı `W10` +%0,96. Hizalanmayan/doğrulanmayan: serbest listeden alınan sayfa hâlâ okunup sıfırlanıyor; indeks taramasıyla `DELETE` ve var olan satır kimliğinde `REPLACE` yazma sırası; `FAST` modunda dosyadaki artık C'den biraz fazla (nedeni incelenmedi); `PRAGMA optimize` yok.
- **Toplam**: `248.30 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.819x`** (disk WAL `0.790x`, bellek-içi `0.932x`)
- **Tepe RSS**: `18,092 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.63`** | `7.59` | `7.53` | **`1.014x`** | `0.987x` | `9.63` | **`0.79x`** | `8,980` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.54`** | `7.51` | `7.66` | **`0.985x`** | `0.807x` | `8.74` | **`0.86x`** | `9,884` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.69`** | `27.50` | `27.66` | **`1.001x`** | `0.748x` | `23.35` | **`1.19x`** | `9,884` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.84`** | `7.71` | `7.82` | **`1.003x`** | `0.546x` | `7.47` | **`1.05x`** | `9,948` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.38`** | `4.35` | `4.41` | **`0.995x`** | `0.254x` | `16.59` | **`0.26x`** | `9,948` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.49`** | `10.44` | `10.48` | **`1.001x`** | `0.962x` | `28.22` | **`0.37x`** | `10,012` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`39.14`** | `38.87` | `38.81` | **`1.008x`** | `1.001x` | `50.35` | **`0.78x`** | `12,336` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.52`** | `9.43` | `9.44` | **`1.009x`** | `0.542x` | `8.05` | **`1.18x`** | `12,336` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.93`** | `11.87` | `11.97` | **`0.997x`** | `0.984x` | `19.06` | **`0.63x`** | `12,336` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.24`** | `11.12` | `11.11` | **`1.012x`** | `0.365x` | `10.94` | **`1.03x`** | `12,584` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`47.20`** | `46.16` | `46.12` | **`1.023x`** | `0.986x` | `57.92` | **`0.81x`** | `18,048` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.54`** | `23.32` | `23.73` | **`0.992x`** | `0.583x` | `24.40` | **`0.96x`** | `18,092` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.92`** | `4.82` | `4.85` | **`1.013x`** | `0.677x` | `4.13` | **`1.19x`** | `18,124` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.25`** | `35.00` | `35.59` | **`0.991x`** | `0.946x` | `26.79` | **`1.32x`** | `18,092` | `fc7490e2b80c68e2` ✓ |

---

## 05h_hizalama_sql — `performans-A@a0cffa4`

**Adım 5h — C'ye hizalama: SQL düzeyi, bedelsiz sınıflar.** SQL üzerinden gözlenen sonuçları C ile eşitler. (A) **Gerçel ↔ metin**: C'nin `sqlite3FpDecode` (17 anlamlı basamak, gidiş-dönüş indirgemesi) ve `sqlite3AtoF` davranışı tüm motorda (`CAST`, `||`, TEXT yakınlığı, `quote`, JSON, CLI); `printf` C'nin `sqlite3_str_vappendf` portu. (B) **Fonksiyonlar**: `substr` (negatif başlangıç, BLOB BLOB kalır), `hex`/`octet_length` (saklanan kodlama), `replace`, `instr`, `trim`, `lower`/`upper`, `round`, `ln`/`log`, `quote`, `like`/`glob`; listede olup uygulanmamış `concat`, `concat_ws`, `unistr`, `unistr_quote` eklendi; `count(DISTINCT)` 1 ile 1.0'ı tek değer sayıyor; `generate_series` metin işlenenli kısıt. (C) **İfade anlambilimi**: R-Tree'ye `INSERT OR FAIL`; satır kimliği 2^63−1'de C'nin rastgele aday algoritması; `sqlite_schema`dan CTAS; `PRAGMA table_info` varsayılan metni; üretilmiş sütun kontrolleri. (D) **`ORDER BY`sız `GROUP BY`** artık C gibi anahtar sırasıyla dönüyor (ölçülen bedel: komut sayısı 1,0000–1,0002). (E) `integrity_check`in üç SQL düzeyi kontrolü daha. (F) **CLI**: `Parse error near line N` + işaretçi satırları, `Error near line N`, hata öncesi satırlar, çıkış kodları, çıktı modlarının biçimi. Onaylanan iki test güncellemesi: `ADD COLUMN` satır kontrolü hatası `SQLITE_ERROR`; `too many columns on sqlite_altertab_t`.

- **Testler**: `cargo test --workspace`: 655 geçti (29 yeni), bilinen 4 başarısız, 337 yok sayıldı. Farksal: 109.942 gerçel değer × 16 SQL yolu + 8 CLI modu (1.019.281 çıktı satırı) C ile fark 0 (önce 754.128); fonksiyon taraması 257.527 çağrıda fark 3.317 (önce 62.648) — kalanların 2.796'sı tarih/saat ailesi (`date.c` port edilmedi, `timediff` yok), 504'ü JSON ayrıntıları. Ayrışma bulucu (234 senaryo): hiçbiri kötüleşmedi; stdout C ile eşit 230 → 233, stderr 228 → 234. **Hız:** toplam 1,005x; komut sayıları aynı, yalnız `W14` +%1,4 (`integrity_check` daha çok kontrol yapıyor). `W06` +%5,6 süre, aynı komut sayısıyla (kod yerleşimi; komut önbelleği ıskası +%14). **Hizalanmayan:** sanal tablolarda `total_changes()`/`changes()` (C gölge tablo yazımlarını sayıyor); tarih/saat fonksiyonları; UTF-16 veritabanında BINARY metin sırası; `PRAGMA optimize`; `integrity_check`te kesin olmayan kayan nokta mesajı ve satırlardan sonra hata; `fts5_locale()`; CLI'de savunmacı kip ve hazırlama zamanı hatasının `Parse error` olarak sınıflanmadığı durumlar (ör. üretilmiş sütun hatası). CLI crate'inin kendi üç testi C'den farklı davranışı doğruladığı için (`-json` tek satır, CSV satır sonu/tırnaklama, çok satırlı dizgede CRLF) o davranışlar bırakıldı.
- **Toplam**: `249.18 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.820x`** (disk WAL `0.792x`, bellek-içi `0.931x`)
- **Tepe RSS**: `18,280 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.71`** | `7.60` | `7.63` | **`1.010x`** | `0.997x` | `9.63` | **`0.80x`** | `9,036` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.68`** | `7.63` | `7.54` | **`1.019x`** | `0.823x` | `8.74` | **`0.88x`** | `9,872` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.81`** | `27.58` | `27.69` | **`1.005x`** | `0.751x` | `23.35` | **`1.19x`** | `9,872` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.78`** | `7.64` | `7.84` | **`0.992x`** | `0.542x` | `7.47` | **`1.04x`** | `10,000` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.09`** | `4.07` | `4.38` | **`0.932x`** | `0.237x` | `16.59` | **`0.25x`** | `10,000` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`11.07`** | `11.04` | `10.49` | **`1.055x`** | `1.016x` | `28.22` | **`0.39x`** | `10,132` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.80`** | `38.41` | `39.14` | **`0.991x`** | `0.992x` | `50.35` | **`0.77x`** | `12,556` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.47`** | `9.42` | `9.52` | **`0.995x`** | `0.539x` | `8.05` | **`1.18x`** | `12,556` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.05`** | `11.99` | `11.93` | **`1.010x`** | `0.994x` | `19.06` | **`0.63x`** | `12,556` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.12`** | `11.08` | `11.24` | **`0.989x`** | `0.361x` | `10.94` | **`1.02x`** | `12,804` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.53`** | `45.49` | `47.20` | **`0.986x`** | `0.973x` | `57.92` | **`0.80x`** | `17,908` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.12`** | `23.62` | `23.54` | **`1.025x`** | `0.597x` | `24.40` | **`0.99x`** | `17,928` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.92`** | `4.61` | `4.92` | **`1.000x`** | `0.677x` | `4.13` | **`1.19x`** | `17,960` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.02`** | `35.76` | `35.25` | **`1.022x`** | `0.967x` | `26.79` | **`1.34x`** | `17,928` | `fc7490e2b80c68e2` ✓ |

---

## 05i_duzeltme_yedekleme — `performans-A@39deab5`

**Adım 5i — doğruluk düzeltmesi: yedekleme API'sinde bağlantı kilitleri (performans amacı yok).** `backup_concurrent_bidirectional_steps_never_deadlock` testi yük altında ara sıra düşüyor ya da takılıyordu (3.000 koşuda 11 hata, 11 takılma; taban `4ccdc64`te de aynı). Motorda kilitlenme yoktu; takılma, testin kendi `Barrier`ında bekleyen iş parçacığıydı. Asıl kusur: `enter_dual` iki bağlantı kapısını deneme-kilidiyle alıyordu ve bir bağlantı anlık kullanımdayken `Backup::init`/`step`/`finish` **C'nin hiç dönmediği** `SQLITE_BUSY` (“database connection is busy”) dönüyordu. C bu üç çağrıda iki bağlantının mutex'ini engelleyerek alır. Artık motor da bekliyor — iki kapıyı her zaman artan bağlantı kimliği sırasıyla (tek küresel sıra), bu yüzden bekleme döngüsü oluşamıyor. İki sızıntı da giderildi: reddedilen bir `finish`/`Blob::close` çağrısı tutamacı kalıcı olarak “kullanımda” bırakıyordu. Ayrıca C'ye hizalananlar: ilk adımdan önce `remaining()`/`pagecount()` 0 (önce −1); yedekleme sürerken kaynağı kapatma `SQLITE_BUSY`; farklı sayfa boyutlu dolu bellek-içi hedef `SQLITE_READONLY`; hedef işlem ilk adımdan itibaren EXCLUSIVE; kopya sonrası hedef şema çerezi.

- **Testler**: `cargo test --workspace`: 669 geçti (14 yeni), bilinen 4 başarısız, 337 yok sayıldı. Özgün test: düzeltme sonrası 4.000 koşu (48 paralel, 30 sn zaman aşımı) — 4.000 geçti, 0 takılma, 0 hata (ajanın ayrıca bildirdiği: 16.000/16.000). Kilit testlerinin 7'si de eski davranış geri konunca düşüyor. **Hız:** toplam 0,989x; `W03`/`W13`/`W01` komut oranları 1,0005 / 0,9999 / 1,0002. **C hakkında not:** aynı senaryo C 3.54.1'in kendisinde iki yönlü yedeklemede kilitleniyor (tek turlu 3.000 koşunun %6'sı; ABBA sırası) — motor bunu taklit etmiyor. **Hizalanmayan (sonraki adıma):** başarısız son adımdan sonra `finish()` (C adımın kodunu döner, motor Ok); salt okunur hedefte C `init`i kabul edip `step`te hata verir (motor `init`te); aynı hedefe ikinci yedekleme (C izin verir); C'nin yeniden başlatmadığı durumlarda yeniden başlatma (`remaining()` dizisi farklı).
- **Toplam**: `249.04 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.823x`** (disk WAL `0.796x`, bellek-içi `0.931x`)
- **Tepe RSS**: `18,268 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.67`** | `7.56` | `7.71` | **`0.996x`** | `0.992x` | `9.63` | **`0.80x`** | `9,088` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.77`** | `7.60` | `7.68` | **`1.011x`** | `0.832x` | `8.74` | **`0.89x`** | `9,992` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.84`** | `27.60` | `27.81` | **`1.001x`** | `0.752x` | `23.35` | **`1.19x`** | `9,992` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.78`** | `7.61` | `7.78` | **`1.001x`** | `0.543x` | `7.47` | **`1.04x`** | `10,056` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.30`** | `4.26` | `4.09` | **`1.051x`** | `0.249x` | `16.59` | **`0.26x`** | `10,056` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.88`** | `10.87` | `11.07` | **`0.983x`** | `0.999x` | `28.22` | **`0.39x`** | `10,184` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.90`** | `38.43` | `38.80` | **`1.002x`** | `0.995x` | `50.35` | **`0.77x`** | `12,672` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.49`** | `9.41` | `9.47` | **`1.002x`** | `0.540x` | `8.05` | **`1.18x`** | `12,672` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.86`** | `11.82` | `12.05` | **`0.984x`** | `0.978x` | `19.06` | **`0.62x`** | `12,672` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.15`** | `11.12` | `11.12` | **`1.003x`** | `0.363x` | `10.94` | **`1.02x`** | `12,856` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.46`** | `45.67` | `46.53` | **`0.998x`** | `0.971x` | `57.92` | **`0.80x`** | `18,156` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.07`** | `23.35` | `24.12` | **`0.998x`** | `0.596x` | `24.40` | **`0.99x`** | `18,268` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`5.03`** | `4.83` | `4.92` | **`1.022x`** | `0.691x` | `4.13` | **`1.22x`** | `18,300` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`35.85`** | `35.65` | `36.02` | **`0.995x`** | `0.963x` | `26.79` | **`1.34x`** | `18,268` | `fc7490e2b80c68e2` ✓ |

---

## 05j_hizalama_cli_yedekleme — `performans-A@225f5c2`

**Adım 5j — C'ye hizalama: komut satırı kabuğu ve yedekleme API'si.** **CLI** C'nin `shell.c.in` ve `qrf.c` dosyalarının portu olarak yeniden yazıldı (biçimlendirici, nokta komutları C'deki sırayla, `.mode` ve seçenekleri, `.dump`, `.schema`, `.import`, `.dbinfo`, komut satırı seçenekleri, `~/.sqliterc`). **Savunmacı kip**: C 3.54.1 kabuğu gibi veritabanları `SQLITE_DBCONFIG_DEFENSIVE` açık açılıyor; motorda bayrak tamamlandı (`journal_mode=OFF` reddi, `schema_version=N` yok sayılır, `writable_schema` etkisiz, gölge tablolara ve `sqlite_schema`ya yazma C'nin hata metinleriyle reddedilir). Onaylanan üç CLI testi C'nin çıktısına göre güncellendi (`-json` satır ayırma, CSV'de CRLF ve tırnaklama, girdi satırlarında CR'nin atılması). **Yedekleme API'si**: başarısız son adımdan sonra `finish()` adımın kodunu döner; salt okunur hedefte `init` kabul edilir, `step` ve `finish` `SQLITE_READONLY` döner; aynı hedefe ikinci yedekleme başlatılabilir; C'nin yeniden başlatmadığı durumlarda (küçülme, günlük kipinde geri alma) yeniden başlatılmaz ve `remaining()`/`pagecount()` dizileri C ile aynıdır. Aynı baytlarla satır değiştirme artık sayfayı kirletmiyor (C'nin `btreeOverwriteContent` davranışı; no-op `UPDATE` değişim sayacını artırmıyor).

- **Testler**: `cargo test --workspace`: 757 geçti (88 yeni), bilinen 4 başarısız, 337 yok sayıldı. **CLI farksalı** (787 durum; stdout + stderr + çıkış kodu C ile aynı): 147 → **722**. Kalan 65: 22 `EXPLAIN`/EQP çıktısı (planlayıcı), 11 PRAGMA satır sırası/içeriği, 16 uygulanmamış nokta komutu ve `.stats`, 6 FTS5 dökümünde yapı kaydı baytı, 3 savunmasız `writable_schema` davranışı, 7 tekil. **Yedekleme** (478 senaryo / 12.442 çağrı; her çağrıdan sonra dönüş kodu, `remaining`, `pagecount`, hedef dosya özeti): farklı çağrı 650 → 307; kalanların tamamı tek bir yedekleme dışı nedenden (bir `CREATE INDEX` sonrası kaynak C'de 86, motorda 87 sayfa). **EQP metni**: 211 sorgunun 132'si (%62,6) C ile aynı; farklar planlayıcı kararları. **Hız:** toplam 1,006x; `W14` komut oranı 0,998. **Uygulanmayan (liste):** 20 kadar nokta komutu (`.expert`, `.sha3sum`, `.load`, `.clone` …), kabuğun kaydettiği SQL fonksiyonları (`sha3`, `regexp`, `readfile` …), etkileşimli (TTY) kip. C'nin belgelerinde tanımsız olan iki durum (yedekleme sırasında hedef bağlantıyı kullanma/kapatma; C'de çökme) yeniden üretilmedi.
- **Toplam**: `248.48 ms` (taban `328.91 ms`, C `295.63 ms`) — geomean `Rust/C` **`0.819x`** (disk WAL `0.794x`, bellek-içi `0.921x`)
- **Tepe RSS**: `18,224 KiB` (C: `6,368 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.58`** | `7.48` | `7.67` | **`0.989x`** | `0.981x` | `9.63` | **`0.79x`** | `9,128` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.57`** | `7.44` | `7.77` | **`0.973x`** | `0.810x` | `8.74` | **`0.86x`** | `10,032` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.22`** | `27.61` | `27.84` | **`1.014x`** | `0.762x` | `23.35` | **`1.21x`** | `10,032` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.89`** | `7.75` | `7.78` | **`1.013x`** | `0.550x` | `7.47` | **`1.06x`** | `10,096` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.27` | `4.30` | **`0.997x`** | `0.248x` | `16.59` | **`0.26x`** | `10,096` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.76`** | `10.72` | `10.88` | **`0.989x`** | `0.988x` | `28.22` | **`0.38x`** | `10,224` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.85`** | `38.47` | `38.90` | **`0.999x`** | `0.993x` | `50.35` | **`0.77x`** | `12,712` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.53`** | `9.49` | `9.49` | **`1.004x`** | `0.543x` | `8.05` | **`1.18x`** | `12,712` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.85`** | `11.84` | `11.86` | **`0.999x`** | `0.977x` | `19.06` | **`0.62x`** | `12,712` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.29`** | `11.24` | `11.15` | **`1.012x`** | `0.367x` | `10.94` | **`1.03x`** | `12,896` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.55`** | `44.97` | `46.46` | **`0.981x`** | `0.952x` | `57.92` | **`0.79x`** | `18,180` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.50`** | `23.18` | `24.07` | **`0.977x`** | `0.582x` | `24.40` | **`0.96x`** | `18,224` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.89`** | `4.55` | `5.03` | **`0.973x`** | `0.673x` | `4.13` | **`1.18x`** | `18,256` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.72`** | `36.28` | `35.85` | **`1.024x`** | `0.986x` | `26.79` | **`1.37x`** | `18,224` | `fc7490e2b80c68e2` ✓ |
