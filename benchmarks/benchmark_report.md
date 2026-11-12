# `sqliters` — Kapsamlı Performans, Saf Rust / SWAR / SIMD / ASM / Morsel / IO / PGO Deney Matrisi ve RAM Raporu

- **Donanım / İS**: `AMD Ryzen 7 8845HS w/ Radeon 780M Graphics` (16 iş parçacığı), `Linux 7.3.3-1-cachyos (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
- **C SQLite Referansı**: `cc (GCC) 16.2.1 20260810` (SQLite `3.54.1`, `speedbench` için `-O3 -DNDEBUG`, `./sqltest` sabit baz için `-O2 MAX_COLUMN=2000`)

## 1. Genel Karşılaştırma Özeti (Tüm Kayıtlı Dallar ve Aşamalar)

| Aşama / Dal | Git (`dal@commit`) | İkili Boyut (`KiB`) | `.text` (`KiB`) | `speedbench` Toplam (`ms`) | `speedbench` Geomean (`vs Saf Rust`) | `speedbench` Geomean (`vs C`) | `speedbench` Tepe RSS (`KiB`) | `./sqltest` Geomean (`vs Saf Rust`) | `./sqltest` Geomean (`vs C`) |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **C SQLite 3.54.1 (`-O3` / `-O2`)** | `referans` | `2,407.8` | `1,876.0` | **`170.11`** | `—` | **`1.000x`** | **`6,120`** | `—` | **`1.000x`** |
| **main** | `main@d70eae4` | `7,460.0` | `5,119.1` | `16,287.95` | `—` | **`18.450x`** | `56,464` | `—` | **`2.837x`** |
| **P1-A** | `performans@dece842` | `7,874.6` | `5,418.6` | `1,212.46` | `—` | **`6.260x`** | `19,184` | `—` | **`2.199x`** |
| **P1-B** | `performans@3eebe56` | `8,068.9` | `5,581.5` | `1,232.57` | `—` | **`3.887x`** | `19,456` | `—` | **`1.454x`** |
| **Saf Rust (performans)** | `performans@da38de4` | `6,385.2` | `5,412.9` | `186.03` | `1.000x` *(baz)* | **`1.164x`** | `17,380` | `1.000x` *(baz)* | **`0.668x`** |
| **SWAR (performans-swar)** | `performans-swar@0a299ef` | `6,396.5` | `5,422.6` | `164.67` | **`0.881x`** | **`1.037x`** | `17,384` | **`0.895x`** | **`0.598x`** |
| **SIMD (performans-simd)** | `performans-simd@7795c5d` | `6,417.5` | `5,441.7` | `152.56` | **`0.829x`** | **`1.027x`** | `17,444` | **`0.798x`** | **`0.533x`** |
| **ASM (performans-asm)** | `performans-asm@c45f2fa` | `6,402.7` | `5,426.6` | `189.39` | **`0.979x`** | **`1.055x`** | `17,516` | **`1.083x`** | **`0.723x`** |
| **Morsel/SSO (performans-morsel)** | `performans-morsel@e0c195d` | `6,756.5` | `5,744.3` | `143.62` | **`0.681x`** | **`0.830x`** | `17,448` | **`0.800x`** | **`0.534x`** |
| **IO/mmap (performans-io-mmap)** | `performans-io-mmap@d4ca6d6` | `6,427.4` | `5,446.9` | `152.85` | **`0.818x`** | **`0.994x`** | `16,620` | **`0.762x`** | **`0.509x`** |
| **PGO/Derleyici (performans-pgo)** | `performans-pgo@eb7d7b2` | `6,953.4` | `6,050.6` | `150.20` | **`0.832x`** | **`1.019x`** | `18,008` | **`0.808x`** | **`0.540x`** |
| **Birleşik (performans-hibrit)** | `performans-hibrit@ccbb4e3` | `7,341.6` | `6,404.0` | `114.86` | **`0.525x`** | **`0.636x`** | `17,124` | **`0.734x`** | **`0.490x`** |
| **performans-1** | `performans-1@ada3b0a` | `7,969.0` | `6,971.8` | `121.62` | **`0.505x`** | **`0.552x`** | `17,928` | **`0.998x`** | **`0.666x`** |

## 2. Toplu `./sqltest` Uçtan Uca CLI Matrisi (`126` Örnek)

| Komut Ailesi (`18` örnek) | C SQLite (`ms`) | main (`ms` / C oranı) | P1-A (`ms` / C oranı) | P1-B (`ms` / C oranı) | Saf Rust (performans) (`ms` / C oranı) | SWAR (performans-swar) (`ms` / C oranı) | SIMD (performans-simd) (`ms` / C oranı) | ASM (performans-asm) (`ms` / C oranı) | Morsel/SSO (performans-morsel) (`ms` / C oranı) | IO/mmap (performans-io-mmap) (`ms` / C oranı) | PGO/Derleyici (performans-pgo) (`ms` / C oranı) | Birleşik (performans-hibrit) (`ms` / C oranı) | performans-1 (`ms` / C oranı) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `34.00` | `106.78` (**3.14x**) | `48.84` (**1.44x**) | `29.96` (**0.88x**) | `29.95` (**0.88x**) | `26.65` (**0.78x**) | `23.59` (**0.69x**) | `31.78` (**0.94x**) | `23.97` (**0.70x**) | `22.42` (**0.66x**) | `23.75` (**0.70x**) | `22.03` (**0.65x**) | `31.46` (**0.93x**) |
| `ingest/cli-persistent-auto` | `15.03` | `25.75` (**1.71x**) | `22.35` (**1.49x**) | `16.47` (**1.10x**) | `15.33` (**1.02x**) | `13.62` (**0.91x**) | `12.37` (**0.82x**) | `15.49` (**1.03x**) | `13.09` (**0.87x**) | `10.98` (**0.73x**) | `12.44` (**0.83x**) | `9.96` (**0.66x**) | `12.36` (**0.82x**) |
| `ingest/cli-persistent-batch` | `21.76` | `104.63` (**4.81x**) | `74.15` (**3.41x**) | `14.74` (**0.68x**) | `9.48` (**0.44x**) | `8.40` (**0.39x**) | `7.87` (**0.36x**) | `11.26` (**0.52x**) | `7.05` (**0.32x**) | `7.25` (**0.33x**) | `8.00` (**0.37x**) | `5.93` (**0.27x**) | `8.16` (**0.38x**) |
| `ingest/cli-read-auto` | `33.86` | `41.66` (**1.23x**) | `37.80` (**1.12x**) | `33.43` (**0.99x**) | `33.34` (**0.98x**) | `30.76` (**0.91x**) | `27.62` (**0.82x**) | `36.23` (**1.07x**) | `27.72` (**0.82x**) | `25.69` (**0.76x**) | `27.00` (**0.80x**) | `25.20` (**0.74x**) | `31.43` (**0.93x**) |
| `ingest/cli-read-batch` | `44.45` | `125.31` (**2.82x**) | `92.76` (**2.09x**) | `34.01` (**0.77x**) | `30.78` (**0.69x**) | `28.51` (**0.64x**) | `25.33` (**0.57x**) | `34.18` (**0.77x**) | `23.56` (**0.53x**) | `23.26` (**0.52x**) | `24.80` (**0.56x**) | `22.93` (**0.52x**) | `28.80` (**0.65x**) |
| `uuid-query/join-index` | `32.12` | `123.96` (**3.86x**) | `116.65` (**3.63x**) | `161.75` (**5.04x**) | `15.85` (**0.49x**) | `13.08` (**0.41x**) | `11.10` (**0.35x**) | `15.47` (**0.48x**) | `11.80` (**0.37x**) | `11.27` (**0.35x**) | `11.49` (**0.36x**) | `12.19` (**0.38x**) | `18.42` (**0.57x**) |
| `uuid-query/join-noindex` | `27.84` | `118.76` (**4.26x**) | `112.31` (**4.03x**) | `153.81` (**5.52x**) | `12.52` (**0.45x**) | `11.63` (**0.42x**) | `10.23` (**0.37x**) | `14.57` (**0.52x**) | `10.89` (**0.39x**) | `11.04` (**0.40x**) | `10.96` (**0.39x**) | `11.14` (**0.40x**) | `16.51` (**0.59x**) |

## 3. Toplu `speedbench` Özellik & Motor API Matrisi (`W01`–`W14`)

| İş Yükü | C `-O3` (`ms`) | main (`ms` / C oranı) | P1-A (`ms` / C oranı) | P1-B (`ms` / C oranı) | Saf Rust (performans) (`ms` / C oranı) | SWAR (performans-swar) (`ms` / C oranı) | SIMD (performans-simd) (`ms` / C oranı) | ASM (performans-asm) (`ms` / C oranı) | Morsel/SSO (performans-morsel) (`ms` / C oranı) | IO/mmap (performans-io-mmap) (`ms` / C oranı) | PGO/Derleyici (performans-pgo) (`ms` / C oranı) | Birleşik (performans-hibrit) (`ms` / C oranı) | performans-1 (`ms` / C oranı) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `5.13` | `50.48` (**8.77x**) | `22.34` (**3.79x**) | `6.24` (**1.13x**) | `3.95` (**0.80x**) | `4.25` (**0.84x**) | `3.80` (**0.77x**) | `3.97` (**0.85x**) | `3.87` (**0.89x**) | `3.13` (**0.72x**) | `3.76` (**0.84x**) | `2.73` (**0.62x**) | `3.18` (**0.62x**) |
| `W02_create_indexes` | `4.59` | `366.70` (**77.55x**) | `43.01` (**10.26x**) | `4.27` (**0.96x**) | `5.53` (**1.24x**) | `4.15` (**0.90x**) | `4.41` (**1.04x**) | `4.49` (**0.91x**) | `2.04` (**0.47x**) | `4.15` (**0.96x**) | `4.30` (**0.99x**) | `1.50` (**0.34x**) | `1.77` (**0.39x**) |
| `W03_pk_point_select` | `18.24` | `47.45` (**3.09x**) | `46.27` (**3.06x**) | `22.36` (**1.32x**) | `21.98` (**1.37x**) | `21.47` (**1.29x**) | `20.65` (**1.30x**) | `23.71` (**1.31x**) | `22.39` (**1.37x**) | `18.30` (**1.11x**) | `19.11` (**1.16x**) | `15.33` (**0.94x**) | `16.77` (**0.92x**) |
| `W04_secondary_idx_range` | `5.29` | `105.29` (**25.04x**) | `79.80` (**19.00x**) | `85.76` (**18.43x**) | `7.34` (**1.67x**) | `6.92` (**1.55x**) | `6.87` (**1.58x**) | `7.80` (**1.65x**) | `5.65` (**1.29x**) | `6.25` (**1.40x**) | `6.01` (**1.36x**) | `4.15` (**0.92x**) | `3.94` (**0.74x**) |
| `W05_full_scan_filter_agg` | `6.90` | `307.02` (**47.84x**) | `307.97` (**48.39x**) | `446.59` (**65.93x**) | `7.66` (**1.17x**) | `6.81` (**1.04x**) | `6.86` (**1.10x**) | `7.44` (**1.04x**) | `0.95` (**0.15x**) | `7.08` (**1.08x**) | `7.11` (**1.09x**) | `0.97` (**0.15x**) | `0.51` (**0.07x**) |
| `W06_join_group_order` | `13.88` | `191.38` (**16.46x**) | `190.38` (**16.33x**) | `247.03` (**18.27x**) | `5.16` (**0.40x**) | `4.58` (**0.38x**) | `4.58` (**0.39x**) | `5.98` (**0.43x**) | `4.50` (**0.37x**) | `4.76` (**0.39x**) | `4.91` (**0.40x**) | `4.87` (**0.40x**) | `5.05` (**0.36x**) |
| `W07_window_functions` | `23.62` | `48.65` (**2.44x**) | `47.05` (**2.35x**) | `66.09` (**2.99x**) | `22.50` (**1.02x**) | `16.61` (**0.79x**) | `16.04` (**0.79x**) | `24.64` (**1.03x**) | `16.26` (**0.77x**) | `16.83` (**0.79x**) | `15.37` (**0.73x**) | `14.54` (**0.69x**) | `12.47` (**0.53x**) |
| `W08_indexed_update_delete` | `6.59` | `262.12` (**51.06x**) | `153.59` (**29.71x**) | `12.17` (**2.07x**) | `10.12` (**1.76x**) | `8.51` (**1.58x**) | `8.85` (**1.71x**) | `9.91` (**1.44x**) | `7.44` (**1.37x**) | `7.38` (**1.37x**) | `8.64` (**1.61x**) | `3.93` (**0.74x**) | `5.32` (**0.81x**) |
| `W09_like_text_scan` | `7.59` | `114.44` (**16.08x**) | `110.83` (**15.36x**) | `137.37` (**17.11x**) | `5.95` (**0.77x**) | `4.40` (**0.60x**) | `4.05` (**0.57x**) | `3.90` (**0.51x**) | `5.48` (**0.74x**) | `5.29` (**0.71x**) | `5.44` (**0.74x**) | `4.01` (**0.55x**) | `3.32` (**0.44x**) |
| `W10_jsonb_extract_agg` | `5.26` | `140.39` (**25.73x**) | `34.59` (**6.45x**) | `28.52` (**5.21x**) | `13.76` (**2.63x**) | `14.06` (**2.53x**) | `11.29` (**2.33x**) | `13.89` (**2.42x**) | `11.49` (**2.32x**) | `12.20` (**2.43x**) | `10.84` (**2.18x**) | `6.87` (**1.38x**) | `3.03` (**0.58x**) |
| `W11_fts5_index_and_bm25` | `28.68` | `14126.06` (**531.23x**) | `22.57` (**0.85x**) | `26.37` (**0.94x**) | `24.42` (**0.86x**) | `22.47` (**0.82x**) | `19.36` (**0.72x**) | `27.64` (**0.95x**) | `20.32` (**0.71x**) | `20.38` (**0.75x**) | `18.66` (**0.69x**) | `18.66` (**0.66x**) | `25.32` (**0.88x**) |
| `W12_rtree_insert_and_bbox` | `11.57` | `351.95` (**33.69x**) | `19.13` (**1.82x**) | `20.05` (**1.65x**) | `19.73` (**1.80x**) | `17.10` (**1.62x**) | `15.25` (**1.45x**) | `18.68` (**1.60x**) | `11.09` (**1.01x**) | `16.69` (**1.54x**) | `15.26` (**1.40x**) | `9.45` (**0.87x**) | `10.82` (**0.94x**) |
| `W13_autocommit_wal_txns` | `6.93` | `11.46` (**2.11x**) | `7.74` (**1.33x**) | `9.10` (**1.43x**) | `7.17` (**1.13x**) | `6.29` (**0.98x**) | `5.66` (**0.98x**) | `6.71` (**0.87x**) | `6.92` (**1.34x**) | `4.73` (**0.84x**) | `5.84` (**1.13x**) | `4.04` (**0.68x**) | `4.34` (**0.63x**) |
| `W14_cli_sql_script` | `25.84` | `164.56` (**6.60x**) | `127.18` (**5.59x**) | `120.66` (**4.62x**) | `30.76` (**1.20x**) | `27.05` (**1.13x**) | `24.91` (**1.11x**) | `30.63` (**1.17x**) | `25.23` (**1.07x**) | `25.67` (**1.12x**) | `24.93` (**1.08x**) | `23.80` (**1.07x**) | `25.78` (**1.00x**) |

---

## Başlangıç Referansı: `main` Dalı (Optimizasyon Öncesi Port)

- **Dal / Commit**: `main@d70eae4`
- **CLI İkili Boyutu**: `7,460.0 KiB` (`.text`: `5,119.1 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `16,287.95 ms` (C `-O3`: `153.19 ms`, Geomean `Rust/C`: **`18.450x`**) | Tepe RSS: **`56,464 KiB`** (C: `6,080 KiB`)
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`2.837x`**

### main — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Medyan (`ms`) | P95 (`ms`) | C Medyan (`ms`) | `Rust / C` | Rust Peak RSS (`KiB`) | C Peak RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | **`106.78`** | `109.09` | `34.00` | **`3.140x`** | `8,956` | `5,172` |
| `ingest/cli-persistent-auto` | **`25.75`** | `26.38` | `15.03` | **`1.714x`** | `7,400` | `4,180` |
| `ingest/cli-persistent-batch` | **`104.63`** | `105.89` | `21.76` | **`4.809x`** | `8,096` | `4,848` |
| `ingest/cli-read-auto` | **`41.66`** | `43.75` | `33.86` | **`1.230x`** | `7,288` | `4,188` |
| `ingest/cli-read-batch` | **`125.31`** | `127.44` | `44.45` | **`2.819x`** | `8,012` | `4,660` |
| `uuid-query/join-index` | **`123.96`** | `125.85` | `32.12` | **`3.859x`** | `26,232` | `8,284` |
| `uuid-query/join-noindex` | **`118.76`** | `120.29` | `27.84` | **`4.265x`** | `25,636` | `7,652` |

### main — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Depolama | Rust (`ms`) | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | C RSS (`KiB`) | C İç Yığın (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`50.48`** | `5.76` | **`8.77x`** | `16,016` | `4,776` | `755` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`366.70`** | `4.73` | **`77.55x`** | `16,048` | `5,444` | `1,473` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`47.45`** | `15.37` | **`3.09x`** | `16,048` | `5,444` | `945` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`105.29`** | `4.21` | **`25.04x`** | `16,048` | `5,444` | `945` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`307.02`** | `6.42` | **`47.84x`** | `16,520` | `5,444` | `946` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`191.38`** | `11.63` | **`16.46x`** | `18,724` | `5,444` | `1,129` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`48.65`** | `19.98` | **`2.44x`** | `18,792` | `5,732` | `2,323` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`262.12`** | `5.13` | **`51.06x`** | `18,696` | `5,732` | `1,032` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`114.44`** | `7.12` | **`16.08x`** | `18,696` | `5,732` | `1,013` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`140.39`** | `5.46` | **`25.73x`** | `18,668` | `5,700` | `1,014` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`14126.06`** | `26.59` | **`531.23x`** | `56,880` | `6,048` | `974` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`351.95`** | `10.45` | **`33.69x`** | `53,152` | `6,048` | `676` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`11.46`** | `5.44` | **`2.11x`** | `53,184` | `6,080` | `368` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`164.56`** | `24.92` | **`6.60x`** | `9,420` | `4,936` | `151` | `fc7490e2b80c68e2` ✓ |

---

## `performans` (Adım P1-A) — Sanal Tablolar (`FTS5`/`R-Tree`), İkili `JSONB` ve Kayıt Karşılaştırma

- **Dal / Commit**: `performans@dece842`
- **CLI İkili Boyutu**: `7,874.6 KiB` (`.text`: `5,418.6 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `1,212.46 ms` (C `-O3`: `150.97 ms`, Geomean `Rust/C`: **`6.260x`**) | Tepe RSS: **`19,184 KiB`** (C: `6,076 KiB`)
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`2.199x`**

### P1-A — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Medyan (`ms`) | P95 (`ms`) | C Medyan (`ms`) | `Rust / C` | Rust Peak RSS (`KiB`) | C Peak RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | **`48.84`** | `52.38` | `34.00` | **`1.436x`** | `9,096` | `5,144` |
| `ingest/cli-persistent-auto` | **`22.35`** | `22.98` | `15.03` | **`1.487x`** | `7,812` | `4,204` |
| `ingest/cli-persistent-batch` | **`74.15`** | `74.98` | `21.76` | **`3.408x`** | `8,236` | `4,712` |
| `ingest/cli-read-auto` | **`37.80`** | `44.04` | `33.86` | **`1.116x`** | `7,548` | `4,180` |
| `ingest/cli-read-batch` | **`92.76`** | `94.98` | `44.45` | **`2.087x`** | `7,980` | `4,556` |
| `uuid-query/join-index` | **`116.65`** | `118.53` | `32.12` | **`3.632x`** | `26,752` | `8,308` |
| `uuid-query/join-noindex` | **`112.31`** | `113.71` | `27.84` | **`4.034x`** | `25,784` | `7,652` |

### P1-A — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Depolama | Rust (`ms`) | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | C RSS (`KiB`) | C İç Yığın (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`22.34`** | `5.89` | **`3.79x`** | `16,208` | `4,796` | `755` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`43.01`** | `4.19` | **`10.26x`** | `16,208` | `5,464` | `1,472` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`46.27`** | `15.13` | **`3.06x`** | `16,208` | `5,464` | `944` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`79.80`** | `4.20` | **`19.00x`** | `13,672` | `5,464` | `944` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`307.97`** | `6.36` | **`48.39x`** | `16,596` | `5,464` | `946` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`190.38`** | `11.66` | **`16.33x`** | `18,500` | `5,464` | `1,128` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`47.05`** | `20.04` | **`2.35x`** | `18,504` | `5,752` | `2,322` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`153.59`** | `5.17` | **`29.71x`** | `18,644` | `5,752` | `1,032` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`110.83`** | `7.21` | **`15.36x`** | `18,644` | `5,752` | `1,013` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`34.59`** | `5.36` | **`6.45x`** | `18,620` | `5,720` | `1,014` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`22.57`** | `26.70` | **`0.85x`** | `19,196` | `6,072` | `974` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`19.13`** | `10.50` | **`1.82x`** | `19,324` | `6,072` | `676` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`7.74`** | `5.80` | **`1.33x`** | `19,356` | `6,104` | `368` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`127.18`** | `22.74` | **`5.59x`** | `9,732` | `4,676` | `151` | `fc7490e2b80c68e2` ✓ |

---

## `performans` (Adım P1-B) — Yoğun Pager/WAL, Ham B-Tree, Sıfır Kilitli `Statement` ve CLI Veri Yükleme

- **Dal / Commit**: `performans@3eebe56`
- **CLI İkili Boyutu**: `8,068.9 KiB` (`.text`: `5,581.5 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `1,232.57 ms` (C `-O3`: `166.02 ms`, Geomean `Rust/C`: **`3.887x`**) | Tepe RSS: **`19,456 KiB`** (C: `6,008 KiB`)
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`1.454x`**

### P1-B — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Medyan (`ms`) | P95 (`ms`) | C Medyan (`ms`) | `Rust / C` | Rust Peak RSS (`KiB`) | C Peak RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | **`29.96`** | `35.41` | `34.00` | **`0.881x`** | `8,556` | `4,972` |
| `ingest/cli-persistent-auto` | **`16.47`** | `17.85` | `15.03` | **`1.096x`** | `7,840` | `4,288` |
| `ingest/cli-persistent-batch` | **`14.74`** | `16.91` | `21.76` | **`0.678x`** | `8,504` | `4,772` |
| `ingest/cli-read-auto` | **`33.43`** | `37.25` | `33.86` | **`0.987x`** | `7,308` | `4,308` |
| `ingest/cli-read-batch` | **`34.01`** | `37.28` | `44.45` | **`0.765x`** | `8,000` | `4,776` |
| `uuid-query/join-index` | **`161.75`** | `179.66` | `32.12` | **`5.036x`** | `26,896` | `8,204` |
| `uuid-query/join-noindex` | **`153.81`** | `169.34` | `27.84` | **`5.524x`** | `26,068` | `7,676` |

### P1-B — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Depolama | Rust (`ms`) | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | C RSS (`KiB`) | C İç Yığın (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`6.24`** | `5.53` | **`1.13x`** | `16,404` | `4,816` | `755` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`4.27`** | `4.46` | **`0.96x`** | `16,404` | `5,484` | `1,472` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`22.36`** | `16.89` | **`1.32x`** | `16,404` | `5,484` | `944` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`85.76`** | `4.65` | **`18.43x`** | `16,404` | `5,484` | `944` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`446.59`** | `6.77` | **`65.93x`** | `16,816` | `5,484` | `946` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`247.03`** | `13.52` | **`18.27x`** | `18,972` | `5,484` | `1,128` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`66.09`** | `22.12` | **`2.99x`** | `18,972` | `5,772` | `2,322` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`12.17`** | `5.88` | **`2.07x`** | `18,972` | `5,772` | `1,032` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`137.37`** | `8.03` | **`17.11x`** | `18,972` | `5,772` | `1,013` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`28.52`** | `5.47` | **`5.21x`** | `19,068` | `5,740` | `1,014` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`26.37`** | `28.08` | **`0.94x`** | `19,700` | `6,092` | `974` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`20.05`** | `12.12` | **`1.65x`** | `19,776` | `6,092` | `676` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`9.10`** | `6.35` | **`1.43x`** | `19,808` | `6,124` | `368` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`120.66`** | `26.14` | **`4.62x`** | `9,500` | `4,708` | `151` | `fc7490e2b80c68e2` ✓ |

---

## `performans` (Adım P1-C) — %100 Güvenli Saf Rust Tavanı (`#![forbid(unsafe_code)]`, SWAR/SIMD/ASM Yok)

- **Dal / Commit**: `performans@da38de4`
- **CLI İkili Boyutu**: `6,385.2 KiB` (`.text`: `5,412.9 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `186.03 ms` (C `-O3`: `161.15 ms`, Geomean `Rust/C`: **`1.164x`**) | Tepe RSS: **`17,380 KiB`** (C: `5,968 KiB`)
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.668x`**

### Saf Rust (performans) — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Medyan (`ms`) | P95 (`ms`) | C Medyan (`ms`) | `Rust / C` | Rust Peak RSS (`KiB`) | C Peak RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | **`29.95`** | `32.94` | `34.00` | **`0.881x`** | `7,568` | `5,080` |
| `ingest/cli-persistent-auto` | **`15.33`** | `18.37` | `15.03` | **`1.020x`** | `7,376` | `4,308` |
| `ingest/cli-persistent-batch` | **`9.48`** | `12.65` | `21.76` | **`0.436x`** | `9,740` | `4,744` |
| `ingest/cli-read-auto` | **`33.34`** | `35.77` | `33.86` | **`0.985x`** | `6,864` | `4,240` |
| `ingest/cli-read-batch` | **`30.78`** | `33.39` | `44.45` | **`0.693x`** | `9,424` | `4,576` |
| `uuid-query/join-index` | **`15.85`** | `17.85` | `32.12` | **`0.493x`** | `12,996` | `8,244` |
| `uuid-query/join-noindex` | **`12.52`** | `15.93` | `27.84` | **`0.450x`** | `11,948` | `7,564` |

### Saf Rust (performans) — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Depolama | Rust (`ms`) | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | C RSS (`KiB`) | C İç Yığın (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`3.95`** | `4.91` | **`0.80x`** | `8,456` | `4,680` | `755` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.53`** | `4.45` | **`1.24x`** | `9,372` | `5,348` | `1,472` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`21.98`** | `16.01` | **`1.37x`** | `9,372` | `5,348` | `944` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.34`** | `4.39` | **`1.67x`** | `9,436` | `5,348` | `944` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`7.66`** | `6.52` | **`1.17x`** | `9,436` | `5,348` | `946` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`5.16`** | `12.80` | **`0.40x`** | `9,500` | `5,348` | `1,128` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`22.50`** | `22.06` | **`1.02x`** | `11,964` | `5,636` | `2,322` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`10.12`** | `5.75` | **`1.76x`** | `11,964` | `5,636` | `1,032` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`5.95`** | `7.68` | **`0.77x`** | `11,964` | `5,636` | `1,013` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`13.76`** | `5.22` | **`2.63x`** | `12,940` | `5,604` | `1,014` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`24.42`** | `28.48` | **`0.86x`** | `17,572` | `6,028` | `974` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`19.73`** | `10.94` | **`1.80x`** | `17,640` | `6,028` | `676` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`7.17`** | `6.33` | **`1.13x`** | `17,672` | `6,060` | `368` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`30.76`** | `25.61` | **`1.20x`** | `8,896` | `4,692` | `151` | `fc7490e2b80c68e2` ✓ |

---

## Dal 1: `performans-swar` — Saf Rust'a Karşı Yazmaç-İçi SWAR Deneyi (`#![forbid(unsafe_code)]`)

- **Dal / Commit**: `performans-swar@0a299ef`
- **CLI İkili Boyutu**: `6,396.5 KiB` (`.text`: `5,422.6 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `164.67 ms` (C `-O3`: `157.11 ms`, Geomean `Rust/C`: **`1.037x`**) | Tepe RSS: **`17,384 KiB`** (C: `5,892 KiB`)
- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: Toplam `186.03 ms` → **`164.67 ms`**, Geomean (`Bu Dal / Saf Rust`): **`0.881x`** (`%+11.9` süre değişimi), Tepe RSS: `17,380 KiB` → **`17,384 KiB`**
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.598x`**, Geomean (`Bu Dal / Saf Rust`): **`0.895x`** (`%+10.5`)

> `performans-swar` dalında `%100 #![forbid(unsafe_code)]` korunarak (sıfır `core::arch`, sıfır `asm!`) 32/64-bit yazmaç-içi SWAR teknikleri doğrudan Saf Rust (`performans`) bazına karşı 15–25 turluk CPU-sabitli ABBA testleriyle ölçüldü; Saf Rust'ı geçen 7 çekirdek tutuldu, üstünlük sağlamayan 4 aday kapsam dışı bırakıldı.

**Saf Rust'a Karşı Üstünlük Sağlayıp Kapsama Alınan Çekirdekler:**
- `SWAR-2a` (`record.rs` — `ST_SMALL_NIBBLES = 0x0000_ff00_8864_3210` 64-bit yazmaç nibble LUT): `0..=11` seri-tip uzunluk eşlemesini dallanmasız yazmaç kaydırmasına indirgedi (`W03` ve `W09` kayıt ofset yürüyüşünde `-%3,5`).
- `SWAR-2b` (`record.rs`, `engine.rs` — `0x8080_8080` / `0x8080_8080_8080_8080` paralel MSB başlık çözümü + big-endian `u64` sıralama öneki): 2–3 sütunlu indeks başlıklarını tek komutta kanıtlayıp `CREATE INDEX` önek paketlemesini hızlandırdı (`W02_create_indexes` `-%12,03`, `W04` `-%2,06`).
- `SWAR-2c` (`record.rs` — `swar_cmp_ascii_binary` / `swar_cmp_ascii_nocase`): Saf ASCII hücre dilimlerinde UTF-8 ön doğrulaması olmadan 8-bayt `u64::from_be_bytes` kelime karşılaştırması (`W08_indexed_update_delete` `-%6,20`, `uuid-query/join-noindex` `-%16,87`).
- `SWAR-3a` (`value.rs` — `swar_fold_ascii_lower_u64` / `swar_cmp_nocase_bytes`): 8 baytlık şeritlerde dallanmasız ASCII harf katlamalı big-endian `Collation::NoCase` karşılaştırması.
- `SWAR-3b` (`functions.rs` — `ContainsAsciiSwar` 64-bit ilk+son bayt `haszero` + paketlenmiş `u64` doğrulama): Skaler BMH tablosundaki seri yükle-kullan adres bağımlılığını kaldırarak `W09_like_text_scan` süresini Saf Rust'a göre `-%19,29` düşürdü (`0,61x` vs C `-O3`).
- `SWAR-4` (`shm/mod.rs` — 64-bayt açılmış çift-kelime `u64` WAL sağlama toplamı): Endian dallanmasını döngü dışına taşıyıp 8 baytlık çift kelime yüklemesiyle `W13_autocommit_wal_txns` süresini `-%4,74` iyileştirdi.
- `SWAR-5` (`fts5.rs` — 64-bit tek geçişli ASCII alfanümerik + büyük-harf SWAR kelime tarayıcısı): Ön `is_ascii()` geçişini kaldırıp `<= 7` bayt tokenları tek yazmaç işleminde çıkararak `W11_fts5_index_and_bm25` süresini `-%2,04` düşürdü (`0,78x` vs C `-O3`).

**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**
- `SWAR-1a` (`json.rs` — 64-bit Mycroft `hasless`/`haszero` düz JSON dize tarayıcısı): OLTP JSON anahtarları/değerleri `3–6` bayt olduğu için skaler döngüye karşı `+%0,12` nötr kaldı (`W10`: `13,30 ms` → `13,32 ms`); geri alındı.
- `SWAR-1b` (`json.rs` — `CompiledJsonArrowPath` paketlenmiş `u64` JSONB anahtar eşitliği): JSONB 1-bayt başlığı anahtar uzunluğunu zaten kodladığı için değişken uzunluklu `u64` paketleme `+%1,08` ek yük getirdi; geri alındı.
- `SWAR-2d` (`record.rs` — `compare_raw_records_with_encoding` çift-kayıt `swar_decode_hdr` sarmalayıcısı): İkili aramaların `%97`'si 0. sütunda ayrıştığından ve `7–9` baytlık kayıtlar `>= 8` sınırında dallanma hatası ürettiğinden `W08`'de `+%3,97` gerileme yaptı; geri alındı.
- `SWAR-6` (`varint.rs` — `>= 3` bayt `get_varint_slow` üzerinde `V-SWARPACK`): Varintlerin `%99,9`'u 1–2 bayt hızlı yolundan çıktığı ve 3 baytlık durumda skaler devam yolu `2` çevrimde bittiği için `14` komutluk kelebek birleştirme `+%1,41` yavaşlattı; geri alındı.

### SWAR (performans-swar) — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `29.95` | **`26.65`** | **`0.890x`** | `34.00` | `0.784x` | `7,568` | `7,876` | `5,100` |
| `ingest/cli-persistent-auto` | `15.33` | **`13.62`** | **`0.888x`** | `15.03` | `0.906x` | `7,376` | `7,212` | `4,336` |
| `ingest/cli-persistent-batch` | `9.48` | **`8.40`** | **`0.887x`** | `21.76` | `0.386x` | `9,740` | `9,420` | `4,896` |
| `ingest/cli-read-auto` | `33.34` | **`30.76`** | **`0.922x`** | `33.86` | `0.908x` | `6,864` | `6,664` | `4,168` |
| `ingest/cli-read-batch` | `30.78` | **`28.51`** | **`0.926x`** | `44.45` | `0.641x` | `9,424` | `9,460` | `4,892` |
| `uuid-query/join-index` | `15.85` | **`13.08`** | **`0.825x`** | `32.12` | `0.407x` | `12,996` | `12,988` | `8,224` |
| `uuid-query/join-noindex` | `12.52` | **`11.63`** | **`0.929x`** | `27.84` | `0.418x` | `11,948` | `11,664` | `7,820` |

### SWAR (performans-swar) — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `3.95` | **`4.25`** | **`1.075x`** | `5.04` | `0.84x` | `8,456` | `8,448` | `4,664` |
| `W02_create_indexes` | `5.53` | **`4.15`** | **`0.750x`** | `4.62` | `0.90x` | `9,372` | `9,360` | `5,332` |
| `W03_pk_point_select` | `21.98` | **`21.47`** | **`0.977x`** | `16.64` | `1.29x` | `9,372` | `9,360` | `5,332` |
| `W04_secondary_idx_range` | `7.34` | **`6.92`** | **`0.942x`** | `4.46` | `1.55x` | `9,436` | `9,452` | `5,332` |
| `W05_full_scan_filter_agg` | `7.66` | **`6.81`** | **`0.890x`** | `6.53` | `1.04x` | `9,436` | `9,452` | `5,332` |
| `W06_join_group_order` | `5.16` | **`4.58`** | **`0.888x`** | `12.06` | `0.38x` | `9,500` | `9,492` | `5,332` |
| `W07_window_functions` | `22.50` | **`16.61`** | **`0.738x`** | `21.03` | `0.79x` | `11,964` | `11,928` | `5,620` |
| `W08_indexed_update_delete` | `10.12` | **`8.51`** | **`0.841x`** | `5.40` | `1.58x` | `11,964` | `11,928` | `5,620` |
| `W09_like_text_scan` | `5.95` | **`4.40`** | **`0.739x`** | `7.27` | `0.60x` | `11,964` | `11,928` | `5,620` |
| `W10_jsonb_extract_agg` | `13.76` | **`14.06`** | **`1.022x`** | `5.57` | `2.53x` | `12,940` | `12,964` | `5,588` |
| `W11_fts5_index_and_bm25` | `24.42` | **`22.47`** | **`0.920x`** | `27.48` | `0.82x` | `17,572` | `17,732` | `6,020` |
| `W12_rtree_insert_and_bbox` | `19.73` | **`17.10`** | **`0.867x`** | `10.58` | `1.62x` | `17,640` | `17,752` | `6,020` |
| `W13_autocommit_wal_txns` | `7.17` | **`6.29`** | **`0.877x`** | `6.39` | `0.98x` | `17,672` | `17,784` | `6,052` |
| `W14_cli_sql_script` | `30.76` | **`27.05`** | **`0.879x`** | `24.04` | `1.13x` | `8,896` | `8,628` | `4,692` |

---

## Dal 2: `performans-simd` — Saf Rust'a Karşı Donanım SIMD (`core::arch` AVX2/SSE2/BMI2) Deneyi

- **Dal / Commit**: `performans-simd@7795c5d`
- **CLI İkili Boyutu**: `6,417.5 KiB` (`.text`: `5,441.7 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `152.56 ms` (C `-O3`: `150.20 ms`, Geomean `Rust/C`: **`1.027x`**) | Tepe RSS: **`17,444 KiB`** (C: `5,824 KiB`)
- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: Toplam `186.03 ms` → **`152.56 ms`**, Geomean (`Bu Dal / Saf Rust`): **`0.829x`** (`%+17.1` süre değişimi), Tepe RSS: `17,380 KiB` → **`17,444 KiB`**
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.533x`**, Geomean (`Bu Dal / Saf Rust`): **`0.798x`** (`%+20.2`)

> `performans-simd` dalında tüm üst katman paketlerinde `#![forbid(unsafe_code)]` korunup sıfır bağımlılıklı yaprak `crates/sqliters-simd` paketi (`#![deny(unsafe_op_in_unsafe_fn)]`, `%100` güvenli Rust API imzaları, sıfır `asm!`, sıfır `u64` SWAR) üzerinden donanım `core::arch::x86_64` (`SSE2`/`AVX2`/`BMI2`) çekirdekleri doğrudan Saf Rust (`performans @ 4ccdc64`) bazına karşı 20 turluk CPU-sabitli (`CPU=2`) ABBA testleriyle ölçüldü; Saf Rust'ı kanıtlı biçimde geçen 4 donanım SIMD çekirdeği tutuldu, üstünlük sağlamayan 3 aday temizce geri alındı.

**Saf Rust'a Karşı Üstünlük Sağlayıp Kapsama Alınan Çekirdekler:**
- `SIMD-1` (`crates/sqliters-simd` → `functions.rs` — `E-02` AVX2 32B / SSE2 16B Muła ilk+son bayt katlamalı alt-dize araması + çakışan kuyruk yüklemesi + `find1_byte`/`find2_byte` joker taraması): Skaler `Box<[usize; 256]>` BMH tablosunu kaldırıp ~43 baytlık metin sütunlarını iki adet 32-baytlık çakışan `AVX2` yüklemesiyle tarayarak `W09_like_text_scan` süresini Saf Rust'a karşı `5,95 ms` → `4,05 ms` (`-%31,9`; izole ABBA: `-%23,45`, `20/20`) düşürdü ve C `-O3` karşısında `0,57x` seviyesine indirdi.
- `SIMD-3` (`crates/sqliters-simd` → `shm/mod.rs` — `E-08` AVX2 4-bağımsız-zincir `T^16` `128 B/tur` kümülatif WAL sağlama toplamı + `SSE2` `T^4` geri dönüşü): Seri `s0`/`s1` bağımlılığını `_mm256_mullo_epi32` + `_mm256_shuffle_epi32` + `_mm256_shuffle_epi8` matris üs alma zincirleriyle kırarak 4 KiB WAL çerçeve sağlamasını hızlandırdı (`W01_bulk_insert_prepared`: `3,80 ms` / `0,77x` C; `W13_autocommit_wal_txns`: `7,17 ms` → `5,66 ms` / `0,98x` C; izole ABBA `-%10,20`, `20/20`).
- `SIMD-4` (`crates/sqliters-simd` → `json.rs` — `simdjson` Stage-1 64-bayt `AVX2`/`SSE2` yapısal bit-maskesi sınıflandırıcısı + satır-içi `SSE2` 16B / `AVX2` 32B düz ASCII JSON dize tarayıcısı): `_mm_cmpgt_epi8(0x20, v)` ve `_mm_cmpeq_epi8` ile kalan girdi tamponunda `>= 16` bayt varken `4–6` baytlık JSON anahtarlarının kapanış tırnağını döngüsüz tek vektör komutunda bularak `W10_jsonb_extract_agg` süresini Saf Rust'a karşı `13,80 ms` → `11,29 ms` (tüm paket ABBA `-%2,95`, `20/20`) ve `W14_cli_sql_script` süresini `28,39 ms` → `24,91 ms` düşürdü.
- `SIMD-5` (`crates/sqliters-simd` → `rtree.rs` — `AVX2`/`SSE2` 2B `f32`/`f64` R-Tree sınırlayıcı kutu kesişimi, IEEE-754 bit-kesin `_mm_min_ps`/`_mm_max_ps` birleşim/kapsama, `choose_subtree` alan genişlemesi, tek geçişli bölme metrikleri ve `_mm_shuffle_epi8` big-endian hücre çözücü): `W12_rtree_insert_and_bbox` süresini Saf Rust'a karşı `18,30 ms` → `15,25 ms` (tüm paket ABBA `-%5,49`, `20/20`) iyileştirdi.

**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**
- `SIMD-2` (`value.rs`, `record.rs`, `fast_read.rs` — `E-02` `AVX2`/`SSE2` `BINARY` & `NOCASE` dilim ve anahtar karşılaştırması): `W02`/`W08` içindeki `6` baytlık anahtarlar (`"C-0042"`) `>= 16B` vektör şeritlerine girmezken, `36` baytlık UUID `BINARY` karşılaştırmaları zaten `glibc`'nin `AVX2` `memcmp` rutinine delege edildiğinden Saf Rust'a karşı kazanç sağlamadı (`W02`: `+%0,32`, `W08`: `-%0,53`, `uuid-query/join-index`: `+%0,30`, `uuid-query/join-noindex`: `-%0,05`); geri alındı.
- `SIMD-6` (`fts5.rs` — `AVX2` 32B / `SSE2` 16B paralel ASCII alfanümerik + büyük-harf bit-maskesi FTS5 kelime ayırıcısı): `W11`'in süresinin `>%99`'u `HashMap` terim listeleri, gölge B-Tree yazımları ve `bm25()` puanlamasında harcandığı için gürültü bandında kaldı (`W11`: `20,83 ms` → `20,80 ms`, `-%0,16`); geri alındı.
- `SIMD-7` (`record.rs`, `varint.rs` — `SSE2` 16B MSB kayıt başlığı maskesi + paralel `u16` önek-toplamı ve `BMI2` `_pext_u64` varint çözümü): `128-bit` yığın yazımını izleyen `16-bit` skaler okumalar Zen 4 üzerinde store-to-load yönlendirme duraksaması yarattı (`W06`: `+%7,03`), yazmasız `_mm_movemask_epi8` varyantı ise `2–4` sütunlu dar kayıtlarda `%99,99` tahminlenen `b0 < 0x80` skaler dalına yenildi (`W03`: `+%1,08`, `W05`: `+%1,17`); geri alındı.

### SIMD (performans-simd) — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `29.95` | **`23.59`** | **`0.787x`** | `34.00` | `0.694x` | `7,568` | `7,960` | `4,972` |
| `ingest/cli-persistent-auto` | `15.33` | **`12.37`** | **`0.807x`** | `15.03` | `0.823x` | `7,376` | `7,336` | `4,340` |
| `ingest/cli-persistent-batch` | `9.48` | **`7.87`** | **`0.830x`** | `21.76` | `0.362x` | `9,740` | `9,624` | `4,764` |
| `ingest/cli-read-auto` | `33.34` | **`27.62`** | **`0.828x`** | `33.86` | `0.816x` | `6,864` | `6,880` | `4,256` |
| `ingest/cli-read-batch` | `30.78` | **`25.33`** | **`0.823x`** | `44.45` | `0.570x` | `9,424` | `9,380` | `4,700` |
| `uuid-query/join-index` | `15.85` | **`11.10`** | **`0.700x`** | `32.12` | `0.346x` | `12,996` | `12,996` | `8,236` |
| `uuid-query/join-noindex` | `12.52` | **`10.23`** | **`0.817x`** | `27.84` | `0.367x` | `11,948` | `11,532` | `7,792` |

### SIMD (performans-simd) — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `3.95` | **`3.80`** | **`0.962x`** | `4.96` | `0.77x` | `8,456` | `8,240` | `4,688` |
| `W02_create_indexes` | `5.53` | **`4.41`** | **`0.797x`** | `4.25` | `1.04x` | `9,372` | `9,152` | `5,356` |
| `W03_pk_point_select` | `21.98` | **`20.65`** | **`0.939x`** | `15.88` | `1.30x` | `9,372` | `9,152` | `5,356` |
| `W04_secondary_idx_range` | `7.34` | **`6.87`** | **`0.936x`** | `4.33` | `1.58x` | `9,436` | `9,216` | `5,356` |
| `W05_full_scan_filter_agg` | `7.66` | **`6.86`** | **`0.895x`** | `6.25` | `1.10x` | `9,436` | `9,276` | `5,356` |
| `W06_join_group_order` | `5.16` | **`4.58`** | **`0.887x`** | `11.67` | `0.39x` | `9,500` | `9,280` | `5,356` |
| `W07_window_functions` | `22.50` | **`16.04`** | **`0.713x`** | `20.18` | `0.79x` | `11,964` | `11,776` | `5,644` |
| `W08_indexed_update_delete` | `10.12` | **`8.85`** | **`0.874x`** | `5.17` | `1.71x` | `11,964` | `11,776` | `5,644` |
| `W09_like_text_scan` | `5.95` | **`4.05`** | **`0.681x`** | `7.12` | `0.57x` | `11,964` | `11,776` | `5,644` |
| `W10_jsonb_extract_agg` | `13.76` | **`11.29`** | **`0.820x`** | `4.85` | `2.33x` | `12,940` | `12,752` | `5,612` |
| `W11_fts5_index_and_bm25` | `24.42` | **`19.36`** | **`0.793x`** | `26.79` | `0.72x` | `17,572` | `17,572` | `6,000` |
| `W12_rtree_insert_and_bbox` | `19.73` | **`15.25`** | **`0.773x`** | `10.50` | `1.45x` | `17,640` | `17,668` | `6,000` |
| `W13_autocommit_wal_txns` | `7.17` | **`5.66`** | **`0.789x`** | `5.75` | `0.98x` | `17,672` | `17,700` | `6,032` |
| `W14_cli_sql_script` | `30.76` | **`24.91`** | **`0.810x`** | `22.49` | `1.11x` | `8,896` | `8,820` | `4,804` |

---

## Dal 3: `performans-asm` — Saf Rust'a Karşı El Yazımı Satır-İçi Assembly (`asm!`) Deneyi

- **Dal / Commit**: `performans-asm@c45f2fa`
- **CLI İkili Boyutu**: `6,402.7 KiB` (`.text`: `5,426.6 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `189.39 ms` (C `-O3`: `172.41 ms`, Geomean `Rust/C`: **`1.055x`**) | Tepe RSS: **`17,516 KiB`** (C: `6,180 KiB`)
- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: Toplam `186.03 ms` → **`189.39 ms`**, Geomean (`Bu Dal / Saf Rust`): **`0.979x`** (`%+2.1` süre değişimi), Tepe RSS: `17,380 KiB` → **`17,516 KiB`**
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.723x`**, Geomean (`Bu Dal / Saf Rust`): **`1.083x`** (`%-8.3`)

> `performans-asm` dalında tüm üst katman paketlerinde ve `crates/sqliters-vfs/src/shm/mod.rs` içinde `#![forbid(unsafe_code)]` korunup sıfır bağımlılıklı yaprak `crates/sqliters-asm` paketi (`#![deny(unsafe_op_in_unsafe_fn)]`, `%100` güvenli Rust API imzaları) içinde yalnızca elle zamanlanmış satır-içi `x86-64` assembly (`core::arch::asm!`, açık `vzeroupper` disiplini, sıfır `core::arch::x86_64` intrinsic, sıfır harici `.s` / `global_asm!`, sıfır `u64` SWAR) kullanılarak 6 çekirdek ailesi doğrudan Saf Rust (`performans @ 4ccdc64`) bazına karşı `CPU=2` 20-turluk ABBA protokolüyle sınandı. Saf Rust'ı kanıtlı geçen 4 çekirdek tutuldu, üstünlük sağlamayan 3 aday geri alındı. (Not: Resmi ölçüm sırasında aktif `rustdesk` oturumu altında `amd-pstate-epp` ~3,2 GHz frekans ölçeklemesi devrede olduğundan canlı C `-O3` referansı da `150,20 ms` → `172,41 ms` ölçeklenmiş; aynı saat hızındaki canlı `Rust / C` geometrik oranı **`1,05x`**, disk WAL oranı **`0,95x`** olmuştur.)

**Saf Rust'a Karşı Üstünlük Sağlayıp Kapsama Alınan Çekirdekler:**
- `ASM-1` (`crates/sqliters-asm` → `functions.rs` — Elle zamanlanmış satır-içi `x86-64` `AVX2` 32B / `SSE2` 16..31B Muła ilk+son bayt katlamalı `LIKE '%lit%'`, `GLOB '*lit*'`, `eq_exact`/`eq_ascii_nocase` ve `memchr1`/`memchr2`): Yayınlanmış desen yazmaçlarını döngü boyunca `ymm`/`xmm` içinde sabit tutup `cmova` çakışan kuyruk okuması + `tzcnt`/`blsr` aday yürüyücüsü + açık `vzeroupper` ile `W09_like_text_scan` süresini Saf Rust'a karşı `-%30,56` (`5,38 ms` → `3,74 ms`, `1,44x`, `10/10` ABBA) düşürdü ve canlı C `-O3` karşısında **`0,51x`** (`3,90 ms` vs `7,68 ms`) seviyesine indirdi (hem SWAR hem SIMD `LIKE` çekirdeklerini geçti).
- `ASM-2` (`crates/sqliters-asm` → `shm/mod.rs` — 4-zincirli `T^16` `128 B/tur` `AVX2` WAL kümülatif sağlama toplamı + `SSE2` `T^4` + `<128B` GP/`movbe` başlık yolu + `vzeroupper`): Doğal ve `vpshufb` bayt-takaslı döngüleri döngü dışında ayırarak `W13_autocommit_wal_txns` süresini Saf Rust'a karşı `-%10,41` (`3,41 ms` → `3,06 ms`, `10/10` ABBA; canlı C `-O3` karşısında **`0,87x`**) ve `W01_bulk_insert_prepared` süresini C karşısında **`0,85x`** seviyesine indirdi.
- `ASM-4` (`crates/sqliters-asm` → `rtree.rs` — 2B `f32`/`f64` R-Tree tam-düğüm `movbe`+`pshufb` hücre çözücü, IEEE-754 `-0.0`/`+0.0` bit-kesin `vxorps`+`vcmpps`+`vblendvps` birleşim, tek geçişli `AVX2` `vcvtps2pd` `choose_subtree` ve dallanmasız 2-genişlikli örtüşme süzgeçleri + `vzeroupper`): `W12_rtree_insert_and_bbox` süresini Saf Rust'a karşı `-%7,71` (`16,39 ms` → `15,12 ms`, `1,084x`, `10/10` ABBA) iyileştirerek `SIMD-5` (`-%5,49`) çekirdeğini de geride bıraktı.
- `ASM-6a` (`crates/sqliters-asm` → `record.rs`, `value.rs` — `<0x80` UTF-8 ASCII geçerlilik denetimini 4B/8B çakışan `movbe` sözcük karşılaştırmasıyla birleştiren `BINARY` ve `SSE2` `pcmpgtb`+`pmovmskb`+`bsf` tabanlı `NOCASE` ham anahtar karşılaştırıcıları; tamsayı yollarında yazmaç taşmasını önlemek için `#[inline(never)]` yaprak sınırı): `SIMD-2`'nin kazanç sağlayamadığı `6` baytlık anahtarlarda (`"C-0042"`) `W02_create_indexes` süresini Saf Rust'a karşı `-%12,04` (`4,15 ms` → `3,65 ms`, `10/10` ABBA; C karşısında **`0,91x`**) ve `W08_indexed_update_delete` süresini `-%7,87` (`7,73 ms` → `7,13 ms`, `10/10` ABBA; C karşısında **`1,44x`**) iyileştirdi.

**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**
- `ASM-3` (`json.rs` — Çok genişlikli `SSE2` 4/8/16B + `AVX2` 32B dizgi tarama ve `AVX2`+`PCLMULQDQ` Stage-1 tırnak paritesi): `W10_jsonb_extract_agg` içindeki ~65 baytlık JSON nesneleri ve `4–8` baytlık anahtarlarda opak `INLINEASM` bloğunun derleyici sabit katlamasını kesmesi nedeniyle Saf Rust'a karşı `+%0,40`..`+%1,39` (`12,44 ms` → `12,50 ms`) geride kaldı; geri alındı (JSON'da derleyiciyle bütünleşen `SIMD-4` intrinsics daha üstün).
- `ASM-5a` & `ASM-5b` (`record.rs`, `engine.rs`, `varint.rs` — Satır-içi `movbe` + `0x8080...` MSB kayıt başlığı / sıralama öneki ve satır-içi `BMI2` `movbe + andn + lzcnt + shlx + pext` varint çözümü): Çağrı sınırı maliyeti sıfırlansa dahi `3–6` baytlık dar başlıklarda taşma muhafazaları (`W02`: `+%7,90`, `W05`: `+%5,71`) ve Zen 4 üzerinde 1–2 baytlık (`>%99`) varintlere karşı `~12` çevrimlik `pext` bağımlılık zinciri + canlı yazmaç baskısı (`W05`: `+%4,36`..`+%4,86`) Saf Rust'ın gerisinde kaldı; geri alındı.
- `ASM-6b` (`fts5.rs` — Satır-içi `AVX2` 32B + `SSE2` 16B `alnum`/`upper` bit-maskesi FTS5 sözcükleyici): `W11` toplam süresinin `<%1`'ini oluşturan ~296 KB metinde 192B sabit yükleme + `tzcnt` bit çıkarma döngüsü skaler taramaya göre `+%1,22` (`20,94 ms` → `21,19 ms`) yavaş kaldı; geri alındı.

### ASM (performans-asm) — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `29.95` | **`31.78`** | **`1.061x`** | `34.00` | `0.935x` | `7,568` | `7,648` | `5,116` |
| `ingest/cli-persistent-auto` | `15.33` | **`15.49`** | **`1.010x`** | `15.03` | `1.031x` | `7,376` | `7,340` | `4,328` |
| `ingest/cli-persistent-batch` | `9.48` | **`11.26`** | **`1.188x`** | `21.76` | `0.518x` | `9,740` | `9,164` | `4,756` |
| `ingest/cli-read-auto` | `33.34` | **`36.23`** | **`1.087x`** | `33.86` | `1.070x` | `6,864` | `6,800` | `4,180` |
| `ingest/cli-read-batch` | `30.78` | **`34.18`** | **`1.110x`** | `44.45` | `0.769x` | `9,424` | `9,388` | `4,760` |
| `uuid-query/join-index` | `15.85` | **`15.47`** | **`0.976x`** | `32.12` | `0.482x` | `12,996` | `12,800` | `8,172` |
| `uuid-query/join-noindex` | `12.52` | **`14.57`** | **`1.163x`** | `27.84` | `0.523x` | `11,948` | `11,676` | `7,680` |

### ASM (performans-asm) — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `3.95` | **`3.97`** | **`1.005x`** | `4.68` | `0.85x` | `8,456` | `8,216` | `4,708` |
| `W02_create_indexes` | `5.53` | **`4.49`** | **`0.812x`** | `4.92` | `0.91x` | `9,372` | `9,132` | `5,376` |
| `W03_pk_point_select` | `21.98` | **`23.71`** | **`1.078x`** | `18.09` | `1.31x` | `9,372` | `9,132` | `5,376` |
| `W04_secondary_idx_range` | `7.34` | **`7.80`** | **`1.062x`** | `4.73` | `1.65x` | `9,436` | `9,196` | `5,376` |
| `W05_full_scan_filter_agg` | `7.66` | **`7.44`** | **`0.971x`** | `7.14` | `1.04x` | `9,436` | `9,196` | `5,376` |
| `W06_join_group_order` | `5.16` | **`5.98`** | **`1.159x`** | `13.83` | `0.43x` | `9,500` | `9,260` | `5,376` |
| `W07_window_functions` | `22.50` | **`24.64`** | **`1.095x`** | `24.00` | `1.03x` | `11,964` | `11,724` | `5,664` |
| `W08_indexed_update_delete` | `10.12` | **`9.91`** | **`0.979x`** | `6.89` | `1.44x` | `11,964` | `11,724` | `5,664` |
| `W09_like_text_scan` | `5.95` | **`3.90`** | **`0.655x`** | `7.68` | `0.51x` | `11,964` | `11,724` | `5,664` |
| `W10_jsonb_extract_agg` | `13.76` | **`13.89`** | **`1.010x`** | `5.74` | `2.42x` | `12,940` | `12,700` | `5,632` |
| `W11_fts5_index_and_bm25` | `24.42` | **`27.64`** | **`1.132x`** | `29.07` | `0.95x` | `17,572` | `17,512` | `6,020` |
| `W12_rtree_insert_and_bbox` | `19.73` | **`18.68`** | **`0.947x`** | `11.64` | `1.60x` | `17,640` | `17,620` | `6,020` |
| `W13_autocommit_wal_txns` | `7.17` | **`6.71`** | **`0.937x`** | `7.74` | `0.87x` | `17,672` | `17,652` | `6,052` |
| `W14_cli_sql_script` | `30.76` | **`30.63`** | **`0.996x`** | `26.27` | `1.17x` | `8,896` | `9,044` | `4,888` |

---

## Dal 4: `performans-morsel` — Sütunsal Mikro-Yığın (Vectorized Batch / SOA) & Sıfır-Tahsis Satır/Değer (`German-String SSO`) Deneyi

- **Dal / Commit**: `performans-morsel@e0c195d`
- **CLI İkili Boyutu**: `6,756.5 KiB` (`.text`: `5,744.3 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `143.62 ms` (C `-O3`: `155.09 ms`, Geomean `Rust/C`: **`0.830x`**) | Tepe RSS: **`17,448 KiB`** (C: `6,108 KiB`)
- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: Toplam `186.03 ms` → **`143.62 ms`**, Geomean (`Bu Dal / Saf Rust`): **`0.681x`** (`%+31.9` süre değişimi), Tepe RSS: `17,380 KiB` → **`17,448 KiB`**
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.534x`**, Geomean (`Bu Dal / Saf Rust`): **`0.800x`** (`%+20.0`)

> `performans-morsel` dalında `%100 #![forbid(unsafe_code)]` ve tek-eksen bilimsel izolasyon korunarak (sıfır `core::arch` SIMD, sıfır `asm!`, sıfır SWAR bit hilesi, sıfır `mmap`/`pwritev` ve sıfır LTO/PGO profil değişikliği) bellek yerleşimi, yığın-içi SSO satır temsili, yeniden kullanılabilir kazıma (`scratch`) tamponları ve sayfa-içi morsel yürütme teknikleri doğrudan Saf Rust (`performans @ 4ccdc64`) bazına karşı CPU=2 sabitli 20 turluk ABBA testleriyle ölçüldü; 6 çekirdek ailesi korunarak `speedbench` genel geometrik ortalaması C SQLite 3.54.1 `-O3` karşısında **`0,83x`** (disk WAL **`0,75x`**, toplam süre **`143,62 ms` vs C `155,09 ms` / `0,93x`**) ve `./sqltest` CLI paketi **`0,534x`** seviyesine indirildi.

**Saf Rust'a Karşı Üstünlük Sağlayıp Kapsama Alınan Çekirdekler:**
- `MORSEL-1` (`value.rs`, `core/lib.rs`, `fast_read.rs`, `window.rs` — Yığın-Tahsisatsız `InlineRow` / 22-bayt UTF-8 SSO `InlineVal` + `ValueRef` + Doğrudan Hücre Çözümleme): `<= 4` sütunlu ve `<= 22` bayt metin/blob içeren satırlarda `Vec<Value>` ve `String` tahsisatlarını kaldırıp `OnceLock<Box<[Value]>>` ile tembel maddileştirme sağladı (`W06_join_group_order`: `0,37x` C, `W07_window_functions`: `0,77x` C ve RSS `-924 KiB`).
- `MORSEL-2` (`btree/lib.rs`, `fast_read.rs` — B-Ağacı Yaprak Üst-Sınır `O(log N)` İkili Arama + Sayfa-İçi `IndexLeafMorsel` Toplama + Grup Koşusu Morseli): Yaprak son hücresi `<= üst_sınır` olduğunda tüm yaprak dilimini tek morsel döngüsünde toplayıp aksi halde `O(log N)` ikili aramayla taramayı anında sonlandırdı ve kapsayıcı indeks grup koşularını ikili aramayla birleştirdi (`W05_full_scan_filter_agg`: `7,66 ms` → **`0,95 ms`**, Saf Rust'a göre **`-%87,6`** ve C `-O3` karşısında **`0,15x` / `6,87x` daha hızlı**; `W04_secondary_idx_range`: `7,34 ms` → **`5,65 ms`**, `1,29x` C).
- `MORSEL-3` (`json.rs`, `functions.rs`, `json_vtab.rs`, `fast_read.rs`, `engine.rs` — Yeniden Kullanılabilir `JSONB` Kazıma Tamponları + Sıfır-Klon `jsonb(?i)` Ekleme/Gruplama/Güncelleme): `SingleRowInsertPlan` üzerinde `JsonbParam1Based`/`JsonbConstOk` kazıma kodlaması, ham yaprak diliminden doğrudan `JSONB` `->>` gruplama morseli, sıfır-tahsisatlı `jsonb_set` güncellemesi ve yeniden kullanılabilir `json_each` imleç satırları eklendi (`W10_jsonb_extract_agg`: `13,76 ms` → **`11,49 ms`**, RSS `-916 KiB`).
- `MORSEL-4` (`rtree.rs`, `engine.rs` — Çözümlenmiş Bellek-İçi R-Ağacı Düğüm Önbelleği + Sıfır-Tahsisatlı Gölge Kayıt Yazımı + Kutu Sorgu Kazıma Havuzu): Yoğun 1-tabanlı düğüm önbelleği + `IntHashMap`, `PersistedRowidBound`, yığın üstü alt-ağaç yolu ve yerinde `f32`/`i64` R* bölme ile big-endian `_node` serileştirmesini commit/savepoint sınırına erteledi; `best_index` üzerinde `Le`/`Ge` artık filtrelerini eleyip sorgu kazıma tamponlarını havuzladı (`W12_rtree_insert_and_bbox`: `19,73 ms` → **`11,09 ms`**, ABBA `-%32,33`, C `-O3` ile **`1,01x` başa baş**).
- `MORSEL-5a` (`btree/page.rs`, `btree/lib.rs`, `vdbe/lib.rs`, `compiler.rs`, `core/lib.rs` — Derlenmiş `SingleRowUpdatePlan` / `SingleRowDeletePlan` + Serbest-Blok Yeniden Kullanımlı Yerinde Yaprak Güncelleme/Silme): Tek ikili aramalı `delete_table_returning_raw_into`, serbest blok geri kazanımlı yerinde yaprak değişimi, değişmeyen sütunların ham Format-3 dilim kopyası ve `ColArith` (`col +/- delta`) doğrudan yürütmesini ekledi (`W08_indexed_update_delete`: `10,12 ms` → **`7,44 ms`**, ABBA `-%25,12`).
- `MORSEL-5b` (`btree/lib.rs`, `engine.rs` — Bitişik Bayt-Arenası `CREATE INDEX` + Bijektif `u64` Sıralama Anahtarları): `10.000` ayrı `Vec<u8>` anahtar tahsisatını tek bitişik `Vec<u8>` arenası ve 16 baytlık `ArenaIndexEntry { prefix, off, len }` dizisine indirgedi; homojen tamsayı ve sabit kısa metin indekslerde sıra koruyucu `u64` anahtarlarıyla karşılaştırma geri çağrısını sıfırladı (`W02_create_indexes`: `5,53 ms` → **`2,04 ms`**, ABBA `-%62,51`, C `-O3` karşısında **`0,47x` / `2,15x` daha hızlı**).
- `MORSEL-6` (`btree/lib.rs`, `driver.rs`, `core/lib.rs`, `cli/main.rs` — Sıfır-Tahsisatlı Sağlıklı Sayfa `integrity_check` + Sıfır-Klon Hazırlama/VM Havuzu + `PeekBuf` CLI Toplu Yürütme): `integrity_check` sayfa doğrulamasını yeniden kullanılabilir kazıma vektörlerine taşıdı, `prepare` başına 2x `Stmt::clone()` ve `tail.to_owned()` kopyalarını kaldırdı, `VdbeVm`/`VdbeWriteScratch` yapılarını bağlantı üzerinde havuzladı ve CLI STDIN tamponundaki ardışık DML/TCL satırlarını tek oturumda birleştirdi (`W14_cli_sql_script`: `30,76 ms` → **`25,23 ms`**, `1,07x` C; `cli-persistent-batch`: **`7,05 ms` / `0,324x` C**).

**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**
- `MORSEL-6a-tek-satır` (`cli/main.rs` — `PeekBuf` STDIN satır birleştirme olmadan yalnızca satır-içi `pure_non_returning_prefix_len` yönlendirmesi): `process_input_stream` STDIN akışını satır satır okuduğu için `2.002` `INSERT` cümlesi yine ayrı oturum kilitleriyle çalışıp `W14` üzerinde yalnızca `-%1,30` (`14,10 ms` → `13,91 ms`) etki üretti; bloklamayan `PeekBuf` tampon-içi çoklu satır birleştirme ile değiştirildi.

### Morsel/SSO (performans-morsel) — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `29.95` | **`23.97`** | **`0.800x`** | `34.00` | `0.705x` | `7,568` | `8,200` | `5,100` |
| `ingest/cli-persistent-auto` | `15.33` | **`13.09`** | **`0.853x`** | `15.03` | `0.871x` | `7,376` | `8,104` | `4,252` |
| `ingest/cli-persistent-batch` | `9.48` | **`7.05`** | **`0.744x`** | `21.76` | `0.324x` | `9,740` | `9,224` | `4,772` |
| `ingest/cli-read-auto` | `33.34` | **`27.72`** | **`0.831x`** | `33.86` | `0.819x` | `6,864` | `7,496` | `4,172` |
| `ingest/cli-read-batch` | `30.78` | **`23.56`** | **`0.765x`** | `44.45` | `0.530x` | `9,424` | `8,836` | `4,772` |
| `uuid-query/join-index` | `15.85` | **`11.80`** | **`0.745x`** | `32.12` | `0.367x` | `12,996` | `13,364` | `8,192` |
| `uuid-query/join-noindex` | `12.52` | **`10.89`** | **`0.870x`** | `27.84` | `0.391x` | `11,948` | `11,964` | `7,696` |

### Morsel/SSO (performans-morsel) — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `3.95` | **`3.87`** | **`0.978x`** | `4.37` | `0.89x` | `8,456` | `8,724` | `4,676` |
| `W02_create_indexes` | `5.53` | **`2.04`** | **`0.368x`** | `4.38` | `0.47x` | `9,372` | `9,304` | `5,344` |
| `W03_pk_point_select` | `21.98` | **`22.39`** | **`1.019x`** | `16.29` | `1.37x` | `9,372` | `9,304` | `5,344` |
| `W04_secondary_idx_range` | `7.34` | **`5.65`** | **`0.769x`** | `4.39` | `1.29x` | `9,436` | `9,368` | `5,344` |
| `W05_full_scan_filter_agg` | `7.66` | **`0.95`** | **`0.124x`** | `6.53` | `0.15x` | `9,436` | `9,368` | `5,344` |
| `W06_join_group_order` | `5.16` | **`4.50`** | **`0.873x`** | `12.11` | `0.37x` | `9,500` | `9,480` | `5,344` |
| `W07_window_functions` | `22.50` | **`16.26`** | **`0.722x`** | `21.14` | `0.77x` | `11,964` | `11,040` | `5,632` |
| `W08_indexed_update_delete` | `10.12` | **`7.44`** | **`0.735x`** | `5.42` | `1.37x` | `11,964` | `11,040` | `5,632` |
| `W09_like_text_scan` | `5.95` | **`5.48`** | **`0.921x`** | `7.35` | `0.74x` | `11,964` | `11,040` | `5,632` |
| `W10_jsonb_extract_agg` | `13.76` | **`11.49`** | **`0.835x`** | `4.96` | `2.32x` | `12,940` | `12,024` | `5,600` |
| `W11_fts5_index_and_bm25` | `24.42` | **`20.32`** | **`0.832x`** | `28.48` | `0.71x` | `17,572` | `17,552` | `5,992` |
| `W12_rtree_insert_and_bbox` | `19.73` | **`11.09`** | **`0.562x`** | `10.95` | `1.01x` | `17,640` | `17,608` | `5,992` |
| `W13_autocommit_wal_txns` | `7.17` | **`6.92`** | **`0.966x`** | `5.18` | `1.34x` | `17,672` | `17,640` | `6,024` |
| `W14_cli_sql_script` | `30.76` | **`25.23`** | **`0.820x`** | `23.54` | `1.07x` | `8,896` | `9,144` | `4,844` |

---

## Dal 5: `performans-io-mmap` — Sıfır-Kopya `mmap` Okuma, Vektörel `pwritev` WAL G/Ç ve Hızlı Oturum Kapısı Deneyi

- **Dal / Commit**: `performans-io-mmap@d4ca6d6`
- **CLI İkili Boyutu**: `6,427.4 KiB` (`.text`: `5,446.9 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `152.85 ms` (C `-O3`: `153.91 ms`, Geomean `Rust/C`: **`0.994x`**) | Tepe RSS: **`16,620 KiB`** (C: `6,108 KiB`)
- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: Toplam `186.03 ms` → **`152.85 ms`**, Geomean (`Bu Dal / Saf Rust`): **`0.818x`** (`%+18.2` süre değişimi), Tepe RSS: `17,380 KiB` → **`16,620 KiB`**
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.509x`**, Geomean (`Bu Dal / Saf Rust`): **`0.762x`** (`%+23.8`)

> `performans-io-mmap` dalında tüm üst katman sandıklarında (`sqliters-types`, `sqliters-pager`, `sqliters-btree`, `sqliters-syntax`, `sqliters-vdbe`, `sqliters-ext`, `sqliters-core`, `sqliters-cli`, `sqliters-tcltest`, kök `sqliters`) ve `crates/sqliters-vfs/src/shm/` altında `%100 #![forbid(unsafe_code)]` korunarak; yalnızca `crates/sqliters-vfs/src/sys/` içindeki izole işletim sistemi çekirdek sınırı (`pwritev`, `mmap`/`munmap`), WAL/SHM autocommit anlık görüntü kilitleme protokolü, tek-sahip hızlı oturum kapısı ve talep-boyutlu bellek/kayıt yapıları optimize edildi (sıfır `core::arch` SIMD, sıfır `asm!`, sıfır SWAR bit hilesi, sıfır `Row`/`Value` SSO veya Morsel sorgu motoru değişikliği, sıfır derleyici/PGO profil değişikliği). Doğrudan dokunulmamış Saf Rust (`performans @ 4ccdc64`) bazına karşı CPU=2 sabitli 20 turluk ABBA testleriyle doğrulanan 5 çekirdek ailesi sonucunda resmi `speedbench` süresinde **`152,85 ms` vs C `-O3` `153,91 ms`** (**`0,99x` genel geomean**, **`0,90x` disk WAL geomean**; `W01`: **`3,13 ms / 0,72x` C**, `W13`: **`4,73 ms / 0,84x` C**, `W03`: **`18,30 ms`**), süreç tepe belleğinde **`17.380 KiB` → `16.620 KiB` (`-760 KiB`)** ve 126 örneklik `./sqltest` CLI paketinde yeni tüm-aşamalar rekoru olan **`0,509x` geomean** elde edildi.

**Saf Rust'a Karşı Üstünlük Sağlayıp Kapsama Alınan Çekirdekler:**
- `IOMMAP-1` (`vfs/src/shm/locks.rs`, `vfs/src/shm/mod.rs`, `pager/src/wal.rs`, `pager/src/lib.rs` — Autocommit WAL `begin_read`/`end_read` Hızlı Kilit ve Değişmeyen `-shm` Anlık Görüntü Kısa-Devresi): Çekişmesiz tek-bağlantılı WAL okuyucularda `WalShm` süreç-içi `Mutex` çift edinimini tek adıma indirdi; `-shm` üzerindeki `WalIndexHdr` (`iChange`, `mxFrame`, `salt`) önbellekteki anlık görüntüyle eşleştiğinde ikili başlık sağlama toplamı ve veritabanı başlığı `pread64` yeniden okumalarını atlayarak C SQLite 3.54.1 çok-süreçli WAL protokolüyle `%100` uyumlu tuttu (`W03_pk_point_select`: ABBA `-%20,89`, resmi `21,98 ms` → **`18,30 ms`**; `W04_secondary_idx_range`: `7,34 ms` → **`6,25 ms`**; tekli WAL CLI sorgusunda `pread64: 5 → 4`).
- `IOMMAP-2` (`vfs/src/sys/unix.rs`, `vfs/src/lib.rs`, `pager/src/wal.rs`, `pager/src/lib.rs`, `types/src/bitvec.rs` — Dağıt-Topla `pwritev` (`iovec`) Sıfır-Kopya WAL Çerçeve Yazımı + Bitişik Checkpoint Yazma Birleştirme + Toplu `WalShm` İndeks Güncellemesi): `IOV_MAX` dilimleme ve kısmi yazma yeniden deneme döngüsüne sahip güvenli `VfsFile::write_vectored_at` (`pwritev`) eklendi; çok sayfalı WAL commit'leri `[24B çerçeve başlığı + 4KiB sayfa]` dilimlerini 4 KiB kopyalama yapmadan tek `pwritev` sistem çağrısıyla yazdı, `checkpoint` ve `commit_phase_one` üzerinde ardışık sayfa koşularını birleştirdi ve `Bitvec::reset_for_size` tahsisatsız sıfırlamayı devreye aldı (`W02` yazma sistem çağrıları `656 → 30` / `-%95,4`; `W08` izole yazma çağrıları `762 → 2` / `-%99,7`; `W13_autocommit_wal_txns`: `7,17 ms` → **`4,73 ms` / `0,84x` C**; `W08`: `10,12 ms` → **`7,38 ms`**).
- `IOMMAP-3` (`vfs/src/sys/unix.rs`, `vfs/src/lib.rs`, `pager/src/lib.rs`, `btree/src/lib.rs`, `core/src/engine.rs` — Sıfır-Kopya `mmap(PROT_READ, MAP_SHARED)` Okuma Penceresi + `PRAGMA [schema.]mmap_size`, `varsayılan=0`): `ReadMapInner` / `ReadMapWindow` (`MAX_MMAP_SIZE = 2_147_418_112`), `truncate`/`close` öncesi zorunlu `munmap` (`SIGBUS` koruması), `sqliters-pager` içinde `%100` güvenli sıfır-kopya `ColdPageSource::Mapped` sayfa ödünç alma yolu ve aktif imleç korumalı `PRAGMA [schema.]mmap_size` desteği eklendi (`PRAGMA mmap_size=67108864` ile soğuk `.db` taramasında `pread64: 254 → 0` / `-%100`, 20 turluk ABBA'da oturum içi tarama `8,20 ms` → `6,28 ms` / `-%23,46`, uçtan uca CLI `11,52 ms` → `9,57 ms` / `-%16,99`).
- `IOMMAP-4` (`core/src/lib.rs`, `core/src/engine.rs`, `core/src/execution_context.rs` — Tek-Sahip Hızlı Oturum Kapısı, Sıfır-`Arc::clone` `Statement::step` ve Tek-DB Autocommit Hızlı Commit): Yeniden giriş denetimini tek TLS erişiminde (`ActiveConnStackGuard::try_push`) birleştirdi, oturum çıkışındaki `SeqCst` bariyerini tek baytlık `gate_state: AtomicU8` (`GATE_BUSY | GATE_PENDING_DROPS`) durum makinesine indirdi, çalışan deyim adımlarında `Arc<StatementInner>` klonlarını ve `PointSelectRowYielded` içindeki `SmallInstVec` kopyasını kaldırdı ve tek veritabanlı autocommit yazmalarda tahsisatsız commit yolunu açtı (`W01_bulk_insert_prepared`: `3,95 ms` → **`3,13 ms` / `0,72x` C**; `./sqltest` `cli-persistent-auto`: `15,33 ms` → **`10,98 ms`**).
- `IOMMAP-5` (`pager/src/lib.rs`, `core/src/vtab.rs`, `core/src/execution_context.rs`, `core/src/lib.rs` — Tek-Kopya `:memory:`/`temp` Sayfa Önbelleği + Talep-Boyutlu `Pager` Yapıları + `OnceLock` Tembel Yerleşik Kayıt Defterleri): Özel `:memory:` ve geçici bellek veritabanlarında ikincil `MemFile` (`MemoryVfs`) kopyasını kaldırarak (`deserialize` ve farklı sayfa boyutlu `Backup` maddileştirmesini koruyarak) `PageCache` önbelleğini tek otoriter kopya yaptı; `tx_journaled` ve `ActivityTracker` yapılarını talep-boyutlu başlattı; yerleşik `VTabRegistry` ve `FunctionRegistry` uzantı kayıtlarını `OnceLock` + `O(1)` isim kontrolü arkasına taşıyarak bağlantı açılışında ~45 yığın tahsisatını ve her yerleşik SQL fonksiyon çağrısındaki `BTreeMap` aramasını ortadan kaldırdı (süreç tepe RSS: `17.380 KiB` → **`16.620 KiB` / `-760 KiB`**).

**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**
- `IOMMAP-3b` (`vfs/src/lib.rs`, `pager/src/lib.rs` — Disk Veritabanlarında Pozitif Varsayılan `default_mmap_size = 64 MiB`): CPU=2 sabitli 20 turluk ABBA testinde sıcak `PageCache` (`-2000`) ve `-wal` çerçeveleriyle çalışan `speedbench` iş yüklerinde `W08_indexed_update_delete` kazancını autocheckpoint sırasındaki çekirdek VMA/TLB yıkımı nedeniyle `-%4,40`'tan (`7,30 ms`) `-%1,34`'e (`7,52 ms`, `stdev=0,51 ms`) geriletti, süreç tepe `VmHWM` değerini `+68 KiB` (`17.452` → `17.520 KiB`) artırdı ve C SQLite 3.54.1'in varsayılan `PRAGMA mmap_size = 0` davranışından saptığı için varsayılan değer `0` (açık `PRAGMA mmap_size=N` ile etkinleştirilebilir) olarak geri alındı.

### IO/mmap (performans-io-mmap) — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `29.95` | **`22.42`** | **`0.749x`** | `34.00` | `0.659x` | `7,568` | `7,948` | `5,212` |
| `ingest/cli-persistent-auto` | `15.33` | **`10.98`** | **`0.716x`** | `15.03` | `0.730x` | `7,376` | `7,432` | `4,172` |
| `ingest/cli-persistent-batch` | `9.48` | **`7.25`** | **`0.765x`** | `21.76` | `0.333x` | `9,740` | `9,472` | `4,852` |
| `ingest/cli-read-auto` | `33.34` | **`25.69`** | **`0.770x`** | `33.86` | `0.759x` | `6,864` | `6,648` | `4,268` |
| `ingest/cli-read-batch` | `30.78` | **`23.26`** | **`0.756x`** | `44.45` | `0.523x` | `9,424` | `9,576` | `4,940` |
| `uuid-query/join-index` | `15.85` | **`11.27`** | **`0.711x`** | `32.12` | `0.351x` | `12,996` | `13,012` | `8,148` |
| `uuid-query/join-noindex` | `12.52` | **`11.04`** | **`0.882x`** | `27.84` | `0.397x` | `11,948` | `12,084` | `7,912` |

### IO/mmap (performans-io-mmap) — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `3.95` | **`3.13`** | **`0.793x`** | `4.38` | `0.72x` | `8,456` | `8,328` | `4,696` |
| `W02_create_indexes` | `5.53` | **`4.15`** | **`0.751x`** | `4.34` | `0.96x` | `9,372` | `9,288` | `5,364` |
| `W03_pk_point_select` | `21.98` | **`18.30`** | **`0.832x`** | `16.51` | `1.11x` | `9,372` | `9,288` | `5,364` |
| `W04_secondary_idx_range` | `7.34` | **`6.25`** | **`0.852x`** | `4.45` | `1.40x` | `9,436` | `9,360` | `5,364` |
| `W05_full_scan_filter_agg` | `7.66` | **`7.08`** | **`0.925x`** | `6.57` | `1.08x` | `9,436` | `9,360` | `5,364` |
| `W06_join_group_order` | `5.16` | **`4.76`** | **`0.923x`** | `12.18` | `0.39x` | `9,500` | `9,424` | `5,364` |
| `W07_window_functions` | `22.50` | **`16.83`** | **`0.748x`** | `21.25` | `0.79x` | `11,964` | `11,576` | `5,652` |
| `W08_indexed_update_delete` | `10.12` | **`7.38`** | **`0.729x`** | `5.40` | `1.37x` | `11,964` | `11,576` | `5,652` |
| `W09_like_text_scan` | `5.95` | **`5.29`** | **`0.890x`** | `7.42` | `0.71x` | `11,964` | `11,576` | `5,652` |
| `W10_jsonb_extract_agg` | `13.76` | **`12.20`** | **`0.886x`** | `5.02` | `2.43x` | `12,940` | `12,056` | `5,620` |
| `W11_fts5_index_and_bm25` | `24.42` | **`20.38`** | **`0.835x`** | `27.06` | `0.75x` | `17,572` | `16,668` | `6,004` |
| `W12_rtree_insert_and_bbox` | `19.73` | **`16.69`** | **`0.846x`** | `10.87` | `1.54x` | `17,640` | `16,676` | `6,004` |
| `W13_autocommit_wal_txns` | `7.17` | **`4.73`** | **`0.660x`** | `5.63` | `0.84x` | `17,672` | `16,708` | `6,036` |
| `W14_cli_sql_script` | `30.76` | **`25.67`** | **`0.835x`** | `22.83` | `1.12x` | `8,896` | `8,912` | `4,840` |

---

## Dal 6: `performans-pgo` — LLVM Profil Güdümlü Optimizasyon (`PGO` + `znver4` + `build-std`/`panic=abort`) Deneyi

- **Dal / Commit**: `performans-pgo@eb7d7b2`
- **CLI İkili Boyutu**: `6,953.4 KiB` (`.text`: `6,050.6 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `150.20 ms` (C `-O3`: `153.38 ms`, Geomean `Rust/C`: **`1.019x`**) | Tepe RSS: **`18,008 KiB`** (C: `6,068 KiB`)
- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: Toplam `186.03 ms` → **`150.20 ms`**, Geomean (`Bu Dal / Saf Rust`): **`0.832x`** (`%+16.8` süre değişimi), Tepe RSS: `17,380 KiB` → **`18,008 KiB`**
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.540x`**, Geomean (`Bu Dal / Saf Rust`): **`0.808x`** (`%+19.2`)

> `performans-pgo` dalında `src/`, `crates/`, `tests/` ve `examples/` altındaki tek bir `.rs` kaynak dosyasına dahi dokunulmadan (`git diff -- '*.rs'` `%100` boş; `%100 #![forbid(unsafe_code)]`, sıfır SWAR, sıfır SIMD, sıfır `asm!`, sıfır Morsel/SSO ve sıfır `mmap`/`pwritev` değişikliği korunarak) yalnızca derleyici arka ucu, 4 aşamalı LLVM Enstrümantasyon Profil Güdümlü Optimizasyonu (`PGO`), skaler genel amaçlı yazmaç (`GPR`) hedeflemesi, `LLD` bağlayıcı (`RELR` yeniden konumlandırma paketleme + `cdsort` çağrı-grafiği sayfa dizilimi) ve makine fonksiyonu soğuk-blok ayrıştırması (`--enable-split-machine-functions`) optimize edildi. Resmi kıyaslamalarda `speedbench` toplam süresi Saf Rust tavanı `186,03 ms`'den **`150,20 ms` (C `-O3` `153,38 ms` karşısında `0,98x` toplam süre, `1,02x` genel geomean, `0,96x` disk WAL geomean; `W07`: **`15,37 ms / 0,73x` C**, `W10`: **`10,84 ms`**, `W11`: **`18,66 ms / 0,69x` C** ile üç yeni tüm-aşamalar Rust rekoru)** seviyesine indi; `./sqltest` CLI paketi **`0,540x` geomean** verirken 7 CLI ailesinin tamamında çocuk süreç Tepe RSS değerleri **`-400 KiB` ile `-500 KiB`** arasında düştü (`cli-read-auto`: **`6.448 KiB`**, `cli-persistent-auto`: **`6.924 KiB`** — `panic = "unwind"` ve Grup 23 UDF panik izolasyonu `%100` korundu).

**Saf Rust'a Karşı Üstünlük Sağlayıp Kapsama Alınan Çekirdekler:**
- `PGO-1` (`tools/build_pgo.py`, `tools/run_speedbench.py` — Tam LLVM IR Enstrümantasyon PGO Boru Hattı: `-C profile-generate` → Bağımsız API + CLI Eğitimi → `llvm-profdata merge` → `-C profile-use`): İzole `target/pgo-gen` dizininde enstrümante edilen motor ve CLI ikilileri, `speedbench`'ten tamamen bağımsız deterministik PRNG tohumu (`0x50474F5F32303236`) ve farklı tablo/şema boyutlarıyla (`WAL` toplu `INSERT`, `CREATE INDEX`, PK nokta `SELECT`, ikincil indeks aralık taraması, `JOIN`+`GROUP BY`+`ORDER BY`, pencere fonksiyonları, indeksli `UPDATE`/`DELETE`, `BMH LIKE`, `JSONB`, `FTS5 + bm25`, `2D R-Tree`, autocommit `WAL` ve CLI `.import`/`.read`/batch) eğitilerek `4.885` fonksiyon ve `158.253` temel blok üzerinden `136,2M` gerçek yürütme sayımı `target/pgo-data/merged.profdata` içinde birleştirildi; Zen 4'ün `32 KiB` L1i önbelleğinde sıcak dallanma düşüşü ve çağrı noktası satır-içi açma kararları doğrulandı (tek başına 20 turluk ABBA: `140,95 ms` → **`128,45 ms` / `-%8,87`**, geomean **`0,9225x` / `-%7,75`**; resmi `W07`: `22,50 → 15,37 ms`, `W10`: `13,76 → 10,84 ms`, `W11`: `24,42 → 18,66 ms`).
- `PGO-2` (`tools/build_pgo.py` — Skaler Zen 4 GPR Uzantı Hedeflemesi: `-C target-feature=+bmi1,+bmi2,+lzcnt,+popcnt,+movbe`): `SSE4.1`/`AVX2`/`AVX-512` küresel olarak etkinleştirildiğinde LLVM 23 SLP vektörleştiricisinin skaler yapı alanlarında ürettiği `14.424` adet çapraz yazmaç-kümesi (`GPR ↔ XMM` `vpinsr`/`vpextr`) transfer cezasını tetiklemeden; Format-3 büyük-uçlu (`big-endian`) tamsayı/başlık çözümlemesi için donanım `movbe`, varint ve `Bitvec` bit maskeleri için `lzcnt`/`tzcnt`/`popcnt`/`bmi1`/`bmi2` komutlarını etkinleştirdi (20 turluk ABBA'da `W08_indexed_update_delete`: `-%13,87`, `W13_autocommit_wal_txns`: `-%10,45`, `W12_rtree_insert_and_bbox`: `-%10,09`).
- `PGO-3` (`tools/build_pgo.py` — `LLD 23.1` Bağlayıcı, `RELR` Göreli Yeniden Konumlandırma Paketleme ve `CDSort` Çağrı-Grafiği Sayfa Yerleşimi: `-C link-arg=-fuse-ld=lld -Wl,-z,pack-relative-relocs -Wl,--gc-sections -Wl,--icf=safe -Wl,--call-graph-profile-sort=cdsort`): PIE dinamik yeniden konumlandırma tablolarını (`.rela.dyn` + `.relr.dyn`) **`91,1 KiB` → `3,6 KiB` (`-%96,0`, `-87,5 KiB`)** düzeyine indirdi; GNU `ld`'nin yoksaydığı LLVM PGO çağrı-grafiği ağırlıklarını `cdsort` (Cache-Directed Sort) ile işleyerek sıcak çağıran-çağrılan zincirlerini bitişik `64B` L1i önbellek satırlarına ve `4 KiB` iTLB sayfalarına yerleştirdi.
- `PGO-4` (`tools/build_pgo.py` — Makine Fonksiyonu Soğuk-Blok / Unwind Ayrıştırması ve L1i-Duyarlı Satır-İçi Eşiği: `-C llvm-args=--enable-split-machine-functions -C llvm-args=--hot-callsite-threshold=900`): Grup 21/23/51/65 `std::panic::catch_unwind` sözleşmesini `%100` koruyarak yürütme sayımı `0` olan tüm `panic = "unwind"` iniş bloklarını (`landing pad`) ve soğuk hata yollarını sıcak fonksiyon gövdelerinden `.text.split.*` soğuk sayfalarına ayırdı ve PGO'nun aşırı `3000` satır-içi açma eşiğini `900` ile sınırlayarak `.text` şişmesini `-183 KiB` (`6.233,6` → `6.050,6 KiB`) geri çekti; İS talep-sayfalamasının (`demand paging`) soğuk sayfaları fiziksel RAM'e hiç yüklememesi sayesinde CLI başlangıç RSS'ini **`5.812` → `5.436 KiB` (`-376 KiB`)** ve 7 `./sqltest` CLI ailesinin tamamında çocuk süreç Tepe RSS'ini **`-400 KiB` ile `-500 KiB` arasında** düşürdü.

**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**
- `PGO-1a/1b/1c/1d` ve `PGO-2b/2d` (`-C target-cpu=znver4`, `znver4,-avx512f`, `x86-64-v3` ve `znver4,-avx512f,-vectorize-slp=false` — Küresel Mikro-Mimari Vektör Hedefleme): CPU=2 sabitli 20 turluk ABBA testlerinde PGO'suz `-C target-cpu=znver4` genel geomeanda **`+%2,15` gerileme** (`140,90` → `143,27 ms`; `W13`: `+%10,33`, `W01`: `+%5,70`), `znver4,-avx512f` **`+%1,60` gerileme** ve `x86-64-v3` **`+%1,70` gerileme** üretti. `objdump -d` kök neden analizinde küresel `SSE4.1`/`AVX2` açıldığında LLVM 23 SLP vektörleştiricisinin skaler `u32`/`u64` yapı alanlarını XMM yazmaçlarına paketleyerek ikili genelinde `vpinsr`/`vpextr`/`vzeroupper` sayısını **`27` → `14.424`** düzeyine çıkardığı ve Zen 4'te `GPR ↔ SIMD` çapraz yazmaç-kümesi gecikmesine yol açtığı kanıtlandı.
- `PGO-3-abort` (`Cargo.toml` `[profile.release] panic = "abort"` ve Nightly `-Z build-std=std,panic_abort`): Gerçek `--release` ikilisinde panikleyen bir kullanıcı tanımlı SQL fonksiyonu (`create_scalar_function` / `create_collation` / `progress_handler`) çağrıldığında `std::panic::catch_unwind` devre dışı kalarak süreci **`SIGABRT` (`EXIT_CODE=134`)** ile çökerttiği ve Grup 21/23/51/65 panik izolasyonu + deyim kayıt noktası (`savepoint`) geri alma sözleşmesini bozduğu kanıtlandı. Ayrıca kurulu `nightly-2026-10-02` üzerinde çevrimdışı `rust-src` bileşeni bulunmadığından `panic = "unwind"` korundu ve soğuk unwind bloklarının RAM etkisi `--enable-split-machine-functions` ile sözleşme dostu biçimde giderildi.
- `PGO-4c` (`-C llvm-args=--hot-cold-split` — LLVM Orta-Katman IR Soğuk Fonksiyon Çıkarma): Makine katmanı `--enable-split-machine-functions` üzerine eklendiğinde soğuk blokları ayrı fonksiyonlara çıkarırken ek argüman aktarım maliyeti ve `+71,3 KiB` ek `.eh_frame` FDE tablosu (`679,2` → `750,5 KiB`) üreterek 20 turluk ABBA'da genel geomeanı `0,9213x` → `0,9293x`'e gerilettiği için geri alındı (`llvm-bolt` / `perf2bolt` ise sistemde kurulu olmadığından kapsam dışı bırakıldı).

### PGO/Derleyici (performans-pgo) — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `29.95` | **`23.75`** | **`0.793x`** | `34.00` | `0.699x` | `7,568` | `7,636` | `5,088` |
| `ingest/cli-persistent-auto` | `15.33` | **`12.44`** | **`0.811x`** | `15.03` | `0.828x` | `7,376` | `6,924` | `4,184` |
| `ingest/cli-persistent-batch` | `9.48` | **`8.00`** | **`0.845x`** | `21.76` | `0.368x` | `9,740` | `9,088` | `4,980` |
| `ingest/cli-read-auto` | `33.34` | **`27.00`** | **`0.810x`** | `33.86` | `0.798x` | `6,864` | `6,448` | `4,236` |
| `ingest/cli-read-batch` | `30.78` | **`24.80`** | **`0.806x`** | `44.45` | `0.558x` | `9,424` | `9,052` | `4,904` |
| `uuid-query/join-index` | `15.85` | **`11.49`** | **`0.725x`** | `32.12` | `0.358x` | `12,996` | `12,564` | `8,292` |
| `uuid-query/join-noindex` | `12.52` | **`10.96`** | **`0.875x`** | `27.84` | `0.394x` | `11,948` | `11,544` | `7,724` |

### PGO/Derleyici (performans-pgo) — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `3.95` | **`3.76`** | **`0.952x`** | `4.45` | `0.84x` | `8,456` | `8,356` | `4,620` |
| `W02_create_indexes` | `5.53` | **`4.30`** | **`0.777x`** | `4.33` | `0.99x` | `9,372` | `9,496` | `5,288` |
| `W03_pk_point_select` | `21.98` | **`19.11`** | **`0.869x`** | `16.54` | `1.16x` | `9,372` | `9,496` | `5,288` |
| `W04_secondary_idx_range` | `7.34` | **`6.01`** | **`0.819x`** | `4.43` | `1.36x` | `9,436` | `9,496` | `5,288` |
| `W05_full_scan_filter_agg` | `7.66` | **`7.11`** | **`0.928x`** | `6.53` | `1.09x` | `9,436` | `9,496` | `5,288` |
| `W06_join_group_order` | `5.16` | **`4.91`** | **`0.952x`** | `12.14` | `0.40x` | `9,500` | `9,560` | `5,288` |
| `W07_window_functions` | `22.50` | **`15.37`** | **`0.683x`** | `21.12` | `0.73x` | `11,964` | `12,048` | `5,576` |
| `W08_indexed_update_delete` | `10.12` | **`8.64`** | **`0.853x`** | `5.35` | `1.61x` | `11,964` | `12,176` | `5,576` |
| `W09_like_text_scan` | `5.95` | **`5.44`** | **`0.915x`** | `7.37` | `0.74x` | `11,964` | `12,176` | `5,576` |
| `W10_jsonb_extract_agg` | `13.76` | **`10.84`** | **`0.788x`** | `4.97` | `2.18x` | `12,940` | `13,152` | `5,544` |
| `W11_fts5_index_and_bm25` | `24.42` | **`18.66`** | **`0.764x`** | `27.06` | `0.69x` | `17,572` | `17,968` | `5,940` |
| `W12_rtree_insert_and_bbox` | `19.73` | **`15.26`** | **`0.774x`** | `10.87` | `1.40x` | `17,640` | `18,004` | `5,940` |
| `W13_autocommit_wal_txns` | `7.17` | **`5.84`** | **`0.816x`** | `5.18` | `1.13x` | `17,672` | `18,036` | `5,972` |
| `W14_cli_sql_script` | `30.76` | **`24.93`** | **`0.810x`** | `23.04` | `1.08x` | `8,896` | `9,100` | `4,852` |

---

## Dal 7: `performans-hibrit` — Saf Rust'a Karşı Üstünlük Kanıtlayan Tüm Yaklaşımların Birleşimi

- **Dal / Commit**: `performans-hibrit@ccbb4e3`
- **CLI İkili Boyutu**: `7,341.6 KiB` (`.text`: `6,404.0 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `114.86 ms` (C `-O3`: `154.39 ms`, Geomean `Rust/C`: **`0.636x`**) | Tepe RSS: **`17,124 KiB`** (C: `6,156 KiB`)
- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: Toplam `186.03 ms` → **`114.86 ms`**, Geomean (`Bu Dal / Saf Rust`): **`0.525x`** (`%+47.5` süre değişimi), Tepe RSS: `17,380 KiB` → **`17,124 KiB`**
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.490x`**, Geomean (`Bu Dal / Saf Rust`): **`0.734x`** (`%+26.6`)

> `performans-hibrit` (Dal 7) dalında `Dal 1`..`Dal 6` (`performans-swar`, `performans-simd`, `performans-asm`, `performans-morsel`, `performans-io-mmap` ve `performans-pgo`) dallarında Saf Rust'a karşı üstünlüğü kanıtlanan tüm kazanan çekirdekler ve çapraz katman sinerji iyileştirmeleri tek bir `%100` Format-3 uyumlu mimaride birleştirildi. Tüm motor, sorgu planlayıcı, B-ağacı, sayfalayıcı, uzantı, CLI ve `-shm` protokol sandıklarında (`sqliters-types`, `sqliters-pager`, `sqliters-btree`, `sqliters-syntax`, `sqliters-vdbe`, `sqliters-ext`, `sqliters-core`, `sqliters-cli`, `sqliters-tcltest`, kök `sqliters` ve `crates/sqliters-vfs/src/shm/{mod,locks,mapping}.rs`) `%100 #![forbid(unsafe_code)]` korundu; donanım ve İS etkileşimleri yalnızca 3 izole güvenli-API yaprak sınırında (`crates/sqliters-vfs/src/sys/`, `crates/sqliters-simd`, `crates/sqliters-asm`; açık `vzeroupper` doğrulamalı) tutuldu. Resmi kıyaslamalarda `performans-hibrit`, C SQLite 3.54.1 `-O3` referansını **14 iş yükünün 12'sinde doğrudan geçerek** `speedbench` genel geometrik ortalamasında **`0,640x`** (**disk WAL `0,573x`**, **`:memory:` `0,92x`**, toplam süre **`114,86 ms` vs C `154,39 ms` / `0,74x`**; `W01`: `0,62x`, `W02`: `0,34x`, `W03`: `0,94x`, `W04`: `0,92x`, `W05`: `0,15x`, `W06`: `0,40x`, `W07`: `0,69x`, `W08`: `0,74x`, `W09`: `0,55x`, `W11`: `0,66x`, `W12`: `0,87x`, `W13`: `0,68x`) ve 126 örneklik `./sqltest` CLI paketinde **7/7 ailede C'yi geçerek `0,490x` geomean (`2,04x` daha hızlı; `cli-persistent-batch: 5,93 ms / 0,273x`)** elde etti.

**Saf Rust'a Karşı Üstünlük Sağlayıp Kapsama Alınan Çekirdekler:**
- `KATMAN-A` (`performans-morsel @ e0c195d` — `MORSEL-1..6` Yığınsız Satır/Değer SSO, Morsel Örtücü İndeks Motoru, Kod Çözülmüş Önbellekler ve Arena `CREATE INDEX`): `<= 4` sütun için sıfır yığın tahsisatlı `InlineRow` + 22 bayt UTF-8/Blob SSO `InlineVal` (`MORSEL-1`), B-ağacı yaprak üst-sınır `O(log N)` ikili araması + sayfa-içi `IndexLeafMorsel` dilim toplaması (`MORSEL-2`, `W05`: **`0,97 ms / 0,15x` C**), yeniden kullanılabilir `JSONB` kazıma tamponları ve ham yaprak `->>` grup morselleri (`MORSEL-3`), kod çözülmüş bellek-içi `f32`/`i64` R-Tree düğüm önbelleği + `IntHashMap` + `best_index` `Le`/`Ge` `omit=true` (`MORSEL-4`), sayfa-içi boş-blok yeniden kullanımlı derlenmiş `SingleRowUpdatePlan`/`SingleRowDeletePlan` + biyoaktif `u64` sıralama anahtarlı bitişik `Vec<u8>` arena `CREATE INDEX` (`MORSEL-5a/5b`, `W02`: **`1,50 ms / 0,34x` C**) ve sıfır-klon `prepare` + havuzlanmış `VdbeVm`/`VdbeWriteScratch` + CLI `PeekBuf` toplu STDIN okuma (`MORSEL-6`, `cli-persistent-batch`: **`5,93 ms / 0,273x` C**).
- `KATMAN-B` (`performans-io-mmap @ d4ca6d6` — `IOMMAP-1..5` Vektörel `pwritev` WAL Yazımı, `mmap`, Hızlı WAL Kilidi, Tek-TLS Oturum Kapısı ve Tek-Kopya `:memory:`): Çekişmesiz `WalShm` `begin_read`/`end_read` + değişmeyen `(iChange, mxFrame, salt)` kısa-devresi (`IOMMAP-1`), `VfsFile::write_vectored_at` (`pwritev`) sıfır-kopya çoklu WAL çerçeve yazımı + birleştirilmiş checkpoint sayfa koşuları + `Bitvec::reset_for_size` (`IOMMAP-2`, `W01`: **`2,73 ms / 0,62x` C**, `W13`: **`4,04 ms / 0,68x` C**), `truncate`/`close` öncesi zorunlu `munmap` korumalı `ReadMapWindow` ve `PRAGMA [schema.]mmap_size` (`IOMMAP-3`), tek-TLS `ActiveConnStackGuard::try_push` + tek baytlık `AtomicU8` oturum kapısı + sıfır-`Arc::clone` `advance_running_fields` (`IOMMAP-4`) ve tek-kopya `:memory:`/`temp` sayfa önbelleği + `OnceLock` tembel `VTabRegistry`/`FunctionRegistry` (`IOMMAP-5`, `:memory:` `W10` RSS: **`11.176 KiB`**).
- `KATMAN-C` (`performans-asm @ c45f2fa`, `performans-simd @ 7795c5d`, `performans-swar @ 0a299ef` — Rejim Bazlı En Hızlı Donanım `ASM`, `SIMD` ve `SWAR` Çekirdekleri): `LIKE`/`GLOB` üzerinde `ASM-1` (`tzcnt`/`blsr` + `cmova` kuyruk) + `< 16B` tamponlar için `SWAR-3b` (`W09`: **`4,01 ms / 0,55x` C**); WAL çerçeve sağlama toplamında açık `vzeroupper` korumalı `ASM-2` (`T^16` 4-zincirli AVX2) + `SWAR-4` 8B kuyruğu; `JSONB` ayrıştırmada `MORSEL-3` kazıma tamponlarının üstüne oturtulan `SIMD-4` (`simdjson` Stage-1 + 16B SSE2 dizge tarayıcı); 2B R-Tree'de `MORSEL-4`'ün yerel `f32` düğüm önbelleği üzerinde çalışan `ASM-4` (AVX2 `choose_subtree` ve SSE2/AVX2 kutu kesişim süzgeci, `W12`: **`9,45 ms / 0,87x` C**); kısa kayıt/metin yollarında `SWAR-2a/2b/2c`, `SWAR-3a`, `SWAR-5` (`FTS5` 8B SWAR ASCII belirteçleyici, `W11`: **`18,66 ms / 0,66x` C**) ve `ASM-6a` (`movbe` `BINARY` / SSE2 `NOCASE` ham anahtar karşılaştırma).
- `KATMAN-D` (`core/src/fast_read.rs`, `core/src/lib.rs`, `btree/src/{lib.rs,page.rs}`, `types/src/{json.rs,record.rs}` — Kalan Darboğazlarda Çapraz Katman Sinerji Cilası): (1) `HashGroupByPlan` içinde örtücü olmayan ikincil indeks tam taraması yerine ardışık `TableFullScan` tercih edilerek `W10` Q1 üzerindeki 5.000 rastgele tablo-rowid geri araması kaldırıldı ve `jsonb_payload_size` 1B başlık kısa-devresi eklendi (`W10`: `13,76` → **`6,87 ms`**); (2) B-ağacı tablo iç/yaprak aramalarına ve ham kayıt karşılaştırmaya 1–2B satır-içi varint + 1B başlık hızlı yolları eklendi (`W03`: **`15,33 ms / 0,94x` C**, `W04`: **`4,15 ms / 0,92x` C**); (3) `execute_single_row_update_plan` içinde değişmeyen ikincil indeks anahtarlarının sil-ekle döngüsü atlandı ve aynı uzunluktaki yerel hücre yükleri için tek aramalı sayfa-içi üzerine yazma (`overwrite_table_leaf_span_in_place`) devreye alındı (`W08`: `10,12` → **`3,93 ms / 0,74x` C**).
- `KATMAN-E` (`tools/build_pgo.py` — Hibrit Eğitimli 4 Aşamalı LLVM PGO + Skaler GPR + `LLD RELR/CDSort` + Soğuk Makine-Fonksiyonu Ayrıştırması): `5.136` fonksiyon ve `170.678` temel bloğu kapsayan bağımsız PGO eğitimi ile skaler GPR bayrakları (`+bmi1,+bmi2,+lzcnt,+popcnt,+movbe`), `LLD` `RELR` paketleme (`93,3 KiB` → **`3,9 KiB` / `-%95,8`**), `cdsort` sayfa yerleşimi ve `--enable-split-machine-functions` birleştirildi; ikili genelinde `vpinsr`/`vpextr` sayısı yalnızca **`3`** seviyesinde tutularak skaler SLP cezası önlendi ve CLI `:memory:` açılış RSS'i **`5.040 KiB`** seviyesine indirildi.

**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**
- `SIMD-6` ve `ASM-6b` (`ext/src/fts5.rs` — 16B SSE2 / `pcmpistri` FTS5 Belirteçleyicileri): Kısa 5–7 baytlık ASCII terimlerde `SWAR-5`'in 8 baytlık GPR kelime kontrolünden daha yavaş kaldığı için dışarıda bırakıldı.
- `ASM-3` (`types/src/json.rs` — Yalıtılmış Satır-İçi Assembly JSON Dizge Tarayıcısı): Kısa 4–8 baytlık JSON anahtarlarında opak `asm!` bloğu LLVM sabit katlamasını engellediği için derleyici içsel fonksiyonlarıyla satır-içi açılabilen `SIMD-4` tercih edildi.
- `IOMMAP-3b` (Pozitif Varsayılan `default_mmap_size = 64 MiB`): Autocheckpoint sırasındaki çekirdek VMA/TLB yıkımı ve C SQLite 3.54.1'in varsayılan `PRAGMA mmap_size = 0` sözleşmesiyle tam uyum gereği varsayılan `0` korundu.
- `PGO-Küresel-AVX2` ve `panic = "abort"` (`-C target-cpu=znver4` / `x86-64-v3` ve `panic = "abort"`): Küresel vektör hedeflemesi skaler motor yapılarında `14.424` adet `GPR ↔ XMM` transfer cezası ürettiği, `panic = "abort"` ise Grup 23 kullanıcı tanımlı SQL fonksiyonu (`UDF`) `std::panic::catch_unwind` panik izolasyon sözleşmesini bozduğu için dışlandı.

### Birleşik (performans-hibrit) — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `29.95` | **`22.03`** | **`0.736x`** | `34.00` | `0.648x` | `7,568` | `7,704` | `5,132` |
| `ingest/cli-persistent-auto` | `15.33` | **`9.96`** | **`0.649x`** | `15.03` | `0.662x` | `7,376` | `7,012` | `4,204` |
| `ingest/cli-persistent-batch` | `9.48` | **`5.93`** | **`0.626x`** | `21.76` | `0.273x` | `9,740` | `8,376` | `4,892` |
| `ingest/cli-read-auto` | `33.34` | **`25.20`** | **`0.756x`** | `33.86` | `0.744x` | `6,864` | `6,724` | `4,256` |
| `ingest/cli-read-batch` | `30.78` | **`22.93`** | **`0.745x`** | `44.45` | `0.516x` | `9,424` | `8,196` | `4,848` |
| `uuid-query/join-index` | `15.85` | **`12.19`** | **`0.769x`** | `32.12` | `0.379x` | `12,996` | `12,372` | `8,248` |
| `uuid-query/join-noindex` | `12.52` | **`11.14`** | **`0.889x`** | `27.84` | `0.400x` | `11,948` | `11,432` | `7,776` |

### Birleşik (performans-hibrit) — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `3.95` | **`2.73`** | **`0.691x`** | `4.42` | `0.62x` | `8,456` | `8,428` | `4,700` |
| `W02_create_indexes` | `5.53` | **`1.50`** | **`0.271x`** | `4.35` | `0.34x` | `9,372` | `9,172` | `5,368` |
| `W03_pk_point_select` | `21.98` | **`15.33`** | **`0.697x`** | `16.32` | `0.94x` | `9,372` | `9,172` | `5,368` |
| `W04_secondary_idx_range` | `7.34` | **`4.15`** | **`0.566x`** | `4.53` | `0.92x` | `9,436` | `9,172` | `5,368` |
| `W05_full_scan_filter_agg` | `7.66` | **`0.97`** | **`0.127x`** | `6.61` | `0.15x` | `9,436` | `9,236` | `5,368` |
| `W06_join_group_order` | `5.16` | **`4.87`** | **`0.943x`** | `12.13` | `0.40x` | `9,500` | `9,368` | `5,368` |
| `W07_window_functions` | `22.50` | **`14.54`** | **`0.646x`** | `20.93` | `0.69x` | `11,964` | `11,016` | `5,656` |
| `W08_indexed_update_delete` | `10.12` | **`3.93`** | **`0.388x`** | `5.33` | `0.74x` | `11,964` | `11,208` | `5,656` |
| `W09_like_text_scan` | `5.95` | **`4.01`** | **`0.675x`** | `7.25` | `0.55x` | `11,964` | `11,208` | `5,656` |
| `W10_jsonb_extract_agg` | `13.76` | **`6.87`** | **`0.499x`** | `4.99` | `1.38x` | `12,940` | `11,176` | `5,624` |
| `W11_fts5_index_and_bm25` | `24.42` | **`18.66`** | **`0.764x`** | `28.48` | `0.66x` | `17,572` | `17,124` | `6,052` |
| `W12_rtree_insert_and_bbox` | `19.73` | **`9.45`** | **`0.479x`** | `10.82` | `0.87x` | `17,640` | `17,168` | `6,052` |
| `W13_autocommit_wal_txns` | `7.17` | **`4.04`** | **`0.564x`** | `5.97` | `0.68x` | `17,672` | `17,200` | `6,084` |
| `W14_cli_sql_script` | `30.76` | **`23.80`** | **`0.774x`** | `22.27` | `1.07x` | `8,896` | `9,248` | `4,908` |

---

## Format Türevi 1: `performans-1` — Hizalı Sabit Ofsetli Kayıt & Yaprak Parmak İzi

- **Dal / Commit**: `performans-1@ada3b0a`
- **CLI İkili Boyutu**: `7,969.0 KiB` (`.text`: `6,971.8 KiB`) | C `-O3` Referans: `2,407.8 KiB` (`.text`: `1,876.0 KiB`)
- **`speedbench` Toplam Süre & Tepe RSS**: `121.62 ms` (C `-O3`: `170.11 ms`, Geomean `Rust/C`: **`0.552x`**) | Tepe RSS: **`17,928 KiB`** (C: `6,120 KiB`)
- **Saf Rust (`performans`) Karşılaştırması (`speedbench`)**: Toplam `186.03 ms` → **`121.62 ms`**, Geomean (`Bu Dal / Saf Rust`): **`0.505x`** (`%+49.5` süre değişimi), Tepe RSS: `17,380 KiB` → **`17,928 KiB`**
- **`./sqltest` 126-Örnek CLI Özeti**: Durum `PASS_SCOPED`, Geomean (`Bu Dal / C`): **`0.666x`**, Geomean (`Bu Dal / Saf Rust`): **`0.998x`** (`%+0.2`)

**Saf Rust'a Karşı Üstünlük Sağlamadığı İçin Kapsam Dışı Bırakılan Adaylar:**
- `ParsedPage` Tam-Açımlı `0x1a` İndeks Yaprak Bölünmesi (`crates/sqliters-btree/src/lib.rs`): 12 baytlık parmak-izi dizin yuvaları yaprak başına sığan indeks hücresi sayısını ~310'dan ~170'e indirdiği için rastgele anahtarlı indeks kurulumlarında `Vec<u8>` tahsisatlı bölünme ek yük yarattı; sıfır tahsisli ham `try_fast_index_insert_or_split_raw` ile değiştirilerek elendi.
- Kısa `< 256 B` `JSONB` Nesnelerinde 8 Baytlık `[ktag_u32, key_rel_u16, val_rel_u16]` Dizin Girişi (`crates/sqliters-types/src/json.rs`): Kısa ~66 B belgelerde belge başına +22 B meta veri ekleyip yaprak doluluğunu düşürdüğü için kısa nesnelerde dışlandı; kısa nesnelerde 2B/anahtar `P3J_OBJECT_COMPACT` kullanıldı.
- Sütunsal / PAX B-Ağacı Yaprak Sayfa Yerleşimleri: Mimari kapsam gereği `performans-1` yalnızca satır-yönelimli `O(1)` kayıtlar, bitişik yaprak anahtar/parmak-izi dizinleri ve `O(1)` ikili `JSONB` ile sınırlandırılıp PAX sayfa-içi sütunsal düzen `performans-2` dalına bırakıldı.

### performans-1 — `./sqltest` CLI Komut Seti (`126` Örnek, Süre & Tepe RAM)

| Komut Ailesi | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C SQLite (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `ingest/cli-import` | `29.95` | **`31.46`** | **`1.050x`** | `34.00` | `0.925x` | `7,568` | `7,912` | `5,132` |
| `ingest/cli-persistent-auto` | `15.33` | **`12.36`** | **`0.806x`** | `15.03` | `0.822x` | `7,376` | `7,548` | `4,240` |
| `ingest/cli-persistent-batch` | `9.48` | **`8.16`** | **`0.862x`** | `21.76` | `0.375x` | `9,740` | `8,908` | `4,892` |
| `ingest/cli-read-auto` | `33.34` | **`31.43`** | **`0.943x`** | `33.86` | `0.928x` | `6,864` | `6,688` | `4,276` |
| `ingest/cli-read-batch` | `30.78` | **`28.80`** | **`0.936x`** | `44.45` | `0.648x` | `9,424` | `8,684` | `4,868` |
| `uuid-query/join-index` | `15.85` | **`18.42`** | **`1.162x`** | `32.12` | `0.574x` | `12,996` | `13,936` | `8,216` |
| `uuid-query/join-noindex` | `12.52` | **`16.51`** | **`1.319x`** | `27.84` | `0.593x` | `11,948` | `11,988` | `7,712` |

### performans-1 — `speedbench` Özellik & Motor İş Yükleri (`W01`–`W14`, Süre & RAM)

| İş Yükü | Saf Rust (`ms`) | Bu Dal (`ms`) | `Bu Dal / Saf Rust` | C `-O3` (`ms`) | `Bu Dal / C` | Saf Rust RSS (`KiB`) | Bu Dal RSS (`KiB`) | C RSS (`KiB`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `3.95` | **`3.18`** | **`0.805x`** | `5.13` | `0.62x` | `8,456` | `8,928` | `4,656` |
| `W02_create_indexes` | `5.53` | **`1.77`** | **`0.320x`** | `4.59` | `0.39x` | `9,372` | `9,672` | `5,324` |
| `W03_pk_point_select` | `21.98` | **`16.77`** | **`0.763x`** | `18.24` | `0.92x` | `9,372` | `9,672` | `5,324` |
| `W04_secondary_idx_range` | `7.34` | **`3.94`** | **`0.536x`** | `5.29` | `0.74x` | `9,436` | `9,704` | `5,324` |
| `W05_full_scan_filter_agg` | `7.66` | **`0.51`** | **`0.066x`** | `6.90` | `0.07x` | `9,436` | `9,712` | `5,324` |
| `W06_join_group_order` | `5.16` | **`5.05`** | **`0.979x`** | `13.88` | `0.36x` | `9,500` | `10,128` | `5,324` |
| `W07_window_functions` | `22.50` | **`12.47`** | **`0.554x`** | `23.62` | `0.53x` | `11,964` | `13,620` | `5,612` |
| `W08_indexed_update_delete` | `10.12` | **`5.32`** | **`0.526x`** | `6.59` | `0.81x` | `11,964` | `13,940` | `5,612` |
| `W09_like_text_scan` | `5.95` | **`3.32`** | **`0.558x`** | `7.59` | `0.44x` | `11,964` | `13,940` | `5,612` |
| `W10_jsonb_extract_agg` | `13.76` | **`3.03`** | **`0.220x`** | `5.26` | `0.58x` | `12,940` | `13,908` | `5,580` |
| `W11_fts5_index_and_bm25` | `24.42` | **`25.32`** | **`1.037x`** | `28.68` | `0.88x` | `17,572` | `18,100` | `5,992` |
| `W12_rtree_insert_and_bbox` | `19.73` | **`10.82`** | **`0.548x`** | `11.57` | `0.94x` | `17,640` | `18,100` | `5,992` |
| `W13_autocommit_wal_txns` | `7.17` | **`4.34`** | **`0.606x`** | `6.93` | `0.63x` | `17,672` | `18,132` | `6,024` |
| `W14_cli_sql_script` | `30.76` | **`25.78`** | **`0.838x`** | `25.84` | `1.00x` | `8,896` | `9,940` | `4,912` |

---

## C SQLite 3.54.1 (`-O3`) Karşılaştırmalı Nihai CPU Süresi ve RAM (`Peak RSS` / `VmRSS`) Kök Neden Analizi

> **Kapsam:** `%100` SQLite Format-3 disk ve çok-süreçli `-wal`/`-shm` protokol uyumluluğunu koruyan birleşik sentez dalı **`performans-hibrit` (`ccbb4e3`)** sonrasında; C SQLite `3.54.1` (`-O3`) referansının **CPU süresinde (`ms`)** veya **bellek izinde (`Peak RSS` / `VmRSS` / iç yığın `KiB`)** önde kaldığı her noktanın ELF bölüm tabloları (`size -A`), canlı çekirdek sayfa haritaları (`/proc/<pid>/smaps`), `ptrace` sistem çağrısı sayaçları ve `SQLITE_STATUS_MEMORY_USED` telemetrisiyle kanıtlı teknik açıklamasıdır.

### 1. Genel Tablo (`performans-hibrit` vs C SQLite 3.54.1)

- **`./sqltest` Uçtan Uca CLI Komut Seti (`126` örnek, `7` aile):** `performans-hibrit`, **7 ailenin 7'sinde de** C SQLite'tan hızlıdır (**`0.490x` geomean — C SQLite'tan `2.04x` daha hızlı**; `cli-persistent-batch`: `0.273x`, `uuid-query/join-index`: `0.379x`, `join-noindex`: `0.400x`).
- **`speedbench` Özellik & Motor API Paketi (`W01`–`W14`, `56/56` `FNV-1a` doğrulamalı):** `performans-hibrit`, **14 iş yükünün 12'sinde** C SQLite 3.54.1 `-O3` referansını doğrudan geçmektedir (**`0.640x` genel geomean**, **`0.573x` disk WAL geomean**, **`0.92x` `:memory:` geomean**, toplam süre **`114.86 ms` vs C `154.39 ms` / `0.74x`**).
- **C SQLite'ın CPU süresinde önde kaldığı iş yükü sayısı yalnızca `2 / 14`'tür:** `W10_jsonb_extract_agg` (`6.87 ms` vs `4.99 ms`, `1.38x`) ve `W14_cli_sql_script` (`23.80 ms` vs `22.27 ms`, `1.07x`).

---

### 2. CPU Süresinde C SQLite 3.54.1 (`-O3`) Önde Kalan Noktaların Kök Nedenleri

#### 2.1 `W10_jsonb_extract_agg` (`6.87 ms` vs C `4.99 ms` — `1.38x`, `+1.88 ms` / `5.026` işlem)
`W10`; `:memory:` üzerinde `5.000` adet `INSERT INTO events VALUES (?1, ?2, jsonb(?3))`, `1` adet tam tablo `payload ->> '$.meta.tag'` `GROUP BY` kümelemesi (`Q1`) ve `25` adet türetilmiş alt-sorgulu `SELECT json_group_array(code) FROM (SELECT payload ->> '$.code' ... ORDER BY id LIMIT 20)` (`Q2`) çalıştırır (`performans` Saf Rust bazındaki `13.76 ms` süresinden `-%50.1` iyileşmiştir). Kalan `+1.88 ms` farkın donanım düzeyindeki iki kaynağı:

1. **Kısa (`~66 B`) JSON Belgelerinde İki Geçişli `Stage-1` Yapısal Bant vs C'nin Tek Geçişli Yerinde DFA Çeviricisi (`~1.10 ms` / belge başına `~220 ns`):**
   - `SIMD-4` (`Stage1JsonParser`), `64 B` `pclmulqdq` + AVX2 ile yapısal indeks bandı üretip ikinci geçişte üst-düğüm uzunluklarını geriye yamayarak (`backpatch`) Format-3 `JSONB` bayt dizisini yazar. Bu mimari `>= 256 B` JSON belgelerinde dallanmasız hız sağlarken, `W10` içindeki ortalama `66 baytlık` kısa JSON metinlerinde iki geçişli bant kurulum maliyeti taşır.
   - C SQLite `sqlite3.c` içindeki `jsonTranslateTextToBlob` ise ham işaretçi aritmetiğiyle tek geçişli sıkı bir durum makinesi (`tight switch/goto`) yürütür; her düğüm için `1 bayt` iyimser başlık ayırıp yalnızca taşma olduğunda `memmove` yapar ve ayrıca `examples/speedbench.rs` tarafında Rust API sınırından gelen sahipli `String` (`Value::Text`) sarmalaması olmadan ham `const char*` işaretçisini doğrudan tüketir.
2. **Türetilmiş Alt-Sorgu (`Q2` `FROM (SELECT ... LIMIT 20)`) Genel `VdbeVm` Yürütmesi (`~0.78 ms`):**
   - `Q1` sorgusu `MORSEL-3` ham yaprak `JSONB` morseli ile C hızına inmişken, `25` kez çalışan `Q2` içindeki sıralı alt-sorgu + dış `json_group_array(code)` birleşimi genel `VdbeVm` türetilmiş tablo materyalizasyon yolundan geçer; C SQLite ise alt-sorgu yazmaçlarını tek `Vdbe` çerçevesinde ham `Mem` dizisi üzerinde sıfır kopyayla paylaşır.

#### 2.2 `W14_cli_sql_script` (`23.80 ms` vs C `22.27 ms` — `1.07x`, `+1.53 ms` / `2.260` SQL deyimi)
`W14`; yeni bir çocuk CLI süreci (`fork + execve`) başlatıp boş bir WAL veritabanı üzerinde `2.260` adet parametresiz (literal değerli) `PRAGMA`, `CREATE TABLE/INDEX`, `INSERT`, `UPDATE`, `DELETE`, `JOIN` ve `GROUP BY` cümlesini `stdin` üzerinden çalıştırır (`performans` bazındaki `30.76 ms`'den `-%22.6` indirilmiştir). Kalan `+1.53 ms` farkın dağılımı:

1. **Tek Seferlik Soğuk Süreç İkili Kod Sayfa Hatası (`Demand-Paging Minor Page Faults` — `~0.95–1.05 ms`):**
   - `/proc/<pid>/smaps` ölçümüne göre `W14` boyunca `sqliters` süreci çalıştırılabilir kod/veri bölümlerinden **`5.748 KiB`** (`1.437` adet `4 KiB` sayfa / `~90` adet Linux `do_fault_around` `64 KiB` kümesi) fiziksel belleğe eşlerken, `2.407,8 KiB` boyutundaki C `sqlite3_o3` yalnızca **`2.104 KiB`** (`526` sayfa) eşler.
   - Fazladan **`+3.644 KiB` (`+911` sayfa)** salt-okunur `.text` eşlemesi, tek seferlik süreç ömründe çekirdek sayfa tablosu kurulumu + iTLB/L2i dolumu olarak **`~1.0 ms`** sabit başlangıç vergisi üretir (nitekim kalıcı oturumda çalışan `./sqltest` `cli-persistent-batch` iş yükünde bu sabit vergi amorti edildiği için `sqliters` **`5.93 ms` vs C `21.76 ms` (`0.273x`)** ile C'den `3.67x` hızlıdır).
2. **Parametresiz `2.260` Ad-Hoc SQL Cümlesinin Güvenli AST Ayrıştırması (`~0.50 ms` / deyim başına `~220 ns`):**
   - `W14` içindeki `2.000+` DML cümlesi `?1` parametreli hazır deyim değil, her satırda farklı sabitler içeren ayrı SQL metinleridir. C SQLite tek geçişli Lemon `LALR(1)` yığın ayrıştırıcısı ve `128 B` sabit `Lookaside` dilimleriyle ham işaretçi düğümleri kurarken, `#![forbid(unsafe_code)]` altındaki `sqliters-syntax` tür-güvenceli AST yapılarını oluşturup derler (deyim başına yalnızca `~220 ns` fark).

---

### 3. RAM Tüketiminde (`Peak RSS` / `VmRSS` / İç Yığın) C SQLite Önde Kalan Noktaların Kilobayt Düzeyinde Kök Neden Analizi

Linux'ta `wait4(ru_maxrss)` ve `/proc/<pid>/smaps` ile ölçülen süreç **Tepe RSS (`Resident Set Size`)** değeri üç bileşenin toplamıdır:
$$\text{VmRSS} = \underbrace{\text{RssFile (ELF } \texttt{.text/.rodata/.eh\_frame}\text{)}}_{\text{Süreçler arası paylaşılan salt-okunur kod}} + \underbrace{\text{RssShmem / Shared Libs}}_{\texttt{libc/libm/-shm}} + \underbrace{\text{RssAnon (Özel Yığın + Yığıt)}}_{\text{Gerçek çalışma zamanı RAM tahsisatı}}$$

| Senaryo / Ölçüm Noktası | `performans-hibrit` (`KiB`) | C SQLite `-O3` (`KiB`) | Toplam Fark (`KiB`) | `exe_file_rss` Farkı (Salt-Okunur Kod) | `shared_libs` Farkı (`libc`/`readline`) | `anon_heap_stack` Farkı (Özel Yığın+Stack) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| **1. Minimal CLI (`:memory:` `SELECT 1;`)** | `5.040` | `4.612` | **`+428`** | `+568` (`2.672` vs `2.104`) | **`-108`** (`2.236` vs `2.344`) | **`-32`** (`132` vs `164`) |
| **2. Uçtan Uca CLI (`W14` Betiği Sonu)** | `9.256` *(wait4: `9.248`)* | `4.976` *(wait4: `4.908`)* | **`+4.280`** | **`+3.644`** (`5.748` vs `2.104` — **farkın `%85,1`'i**) | **`-52`** (`2.380` vs `2.432`) | **`+688`** (`1.128` vs `440` — **farkın `%16,1`'i**) |
| **3. İzole `W01` (`10k` Disk WAL `INSERT`)** | `8.460` | `4.660` *(C heap: `755`)* | **`+3.800`** | `~+3.500` | `~0` | `~+300` |
| **4. İzole `W10` (`JSONB` `:memory:`)** | `8.064` | `4.236` *(C heap: `454`)* | **`+3.828`** | `~+3.430` | `~0` | `~+400` (`IOMMAP-5` tek-kopya önbellek) |
| **5. İzole `W12` (`R-Tree` `:memory:`)** | `8.720` | `4.260` *(C heap: `406`)* | **`+4.460`** | `~+3.500` | `~0` | `~+960` (`MORSEL-4` yerel `f32` düğüm önbelleği → `0.87x` C hız) |
| **6. Tam `speedbench` Paketi (`W01`..`W14` Sıralı)** | `17.124` | `6.156` *(C heap tepe: `2.322`)* | **`+10.968`** | `~+4.300` (`W01`..`W14` tüm motor kod sayfaları) | `~0` | `~+6.670` (`W11` `FTS5` ikili indeks + `ptmalloc2` arena tutumu) |

#### Temel Mimari Nedenler

1. **Salt-Okunur ELF `.text` ve Unwind Tablosu Boyutu (`+3,5 MiB` – `+4,3 MiB` `RssFile`):**
   - `sqliters` ikilisi (`7.341,6 KiB` toplam: `6.404,0 KiB` `.text`, `680,5 KiB` `.eh_frame + .gcc_except_table`) C `sqlite3_o3` ikilisinden (`2.407,8 KiB` toplam: `1.876,0 KiB` `.text`, `158,1 KiB` `.eh_frame`, `0 B` `.gcc_except_table`) **`4.933,9 KiB` daha büyüktür**.
   - Bunun nedeni Rust'ın jenerik monomorfizasyonu, `#![forbid(unsafe_code)]` sınır kontrolleri, hem genel `VdbeVm` hem özelleşmiş `fast_read` / morsel / `SIMD` / `ASM` yürütme yollarının birlikte derlenmesi, Rust `std` biçimlendirme/panik altyapısı ve Grup 23 UDF `std::panic::catch_unwind` sözleşmesi için zorunlu olan DWARF LSDA (`.gcc_except_table`) tablolarıdır.
   - `PGO-4` (`--enable-split-machine-functions` + `LLD cdsort`) soğuk hata/unwind bloklarını `.text.split.*` sayfalarına ayırdığı için minimal sorgularda (`SELECT 1;`) bu kodun yalnızca `2.672 KiB`'ı RAM'e yüklenir ve `sqliters` özel yığın+stack alanında C'den **`32 KiB` daha az** RAM kullanır; ancak `W01`–`W14` gibi veritabanı motorunun **tüm alt sistemlerini** (WAL, B-Tree, Window, LIKE, JSONB, FTS5, R-Tree, CLI) tek süreçte çalıştıran bir pakette sıcak `.text` sayfalarının tamamı doğal olarak eşlenir. İşletim sistemi bu `RssFile` sayfalarını tüm eşzamanlı süreçler arasında fiziksel RAM'de **tek kopya** olarak paylaşır ve bellek baskısında diske yazmadan anında geri alabilir.
2. **Çift Katmanlı `FTS5` Bellek-İçi Ters İndeks + Format-3 Gölge Tabloları ve `glibc ptmalloc2` Arena Tutumu (`W11` → `17.124 KiB` Tepe RSS):**
   - İzole ölçümlerde `W01`, `W10`, `W12`, `W13` iş yüklerinin tamamı **`7,7 MiB – 8,7 MiB`** aralığında kalmaktadır. Paket Tepe RSS'ini `17.124 KiB` seviyesine çıkaran tek aşama **`W11_fts5_index_and_bm25`** (`3.000` belge, `~36.000` terim geçişi) iş yüküdür:
   - `%100` Format-3 gölge tablo uyumluluğunu (`_data`, `_idx`, `_content`, `_docsize`, `_config`) korurken `bm25()` sıralamalı aramalarda C SQLite'tan **`%34` daha hızlı (`18.66 ms` vs `28.48 ms` C, `0.66x`)** çalışabilmek için `fts5.rs`, sıkıştırılmış Format-3 segmentlerinin yanında canlı bellek-içi ters indeks (`HashMap<String, Vec<Posting>>` + belge uzunluk haritaları, `~6,5 MiB`) tutar; C SQLite ise yalnızca sıkıştırılmış varint segmentlerini `974 KiB` yığın içinde tarar.
   - `W11` sonunda bağlantı kapatıldığında Rust tüm `Fts5Table` ve `:memory:` yapılarını eksiksiz `Drop` eder; ancak Linux `glibc` (`ptmalloc2`) küçük-blok yığın (`brk`) sayfalarını işletim sistemine hemen `madvise(MADV_DONTNEED)` ile iade etmeyip süreç ömrü boyunca önbellekte tuttuğu için `wait4(ru_maxrss)` yüksek su izi `17.124 KiB` olarak kaydedilir.
3. **Sabit `128 B` `Lookaside` Havuzu vs Tür-Güvenceli Rust Şema ve Sayfa Dizini (`+300` – `+688 KiB` `RssAnon`):**
   - C SQLite bağlantı başına `128 B * 100 = 12,5 KiB` boyutunda tek bir bitişik `Lookaside` tamponu ayırıp şema düğümlerini ve `24 baytlık` `VdbeOp` komutlarını ham işaretçi birleşimleriyle (`union`) bu tampona sıkıştırır.
   - `sqliters` ise `#![forbid(unsafe_code)]` altında tür-güvenceli `Schema`/`Table`/`Index` yapılarını, `O(1)` yoğun `PageCache` dizinini (`slots` + `lru_nodes`), `PeekBuf` toplu CLI tamponunu (`64 KiB`) ve derlenmiş hızlı okuma/yazma plan havuzlarını tutar; bu `~688 KiB` özel yığın yatırımı doğrudan **`0.57x` disk WAL** ve **`0.49x` CLI** hız üstünlüğünü satın almaktadır.

---

## Güncel En Hızlılar Listesi (Tüm Aşamalar Arası Dinamik Liderlik Tablosu)

> **Otomatik Güncellenen Özet:** Bu bölüm her yeni aşama eklendiğinde tüm kayıtlı JSON ölçümlerinden otomatik yeniden üretilir. Her hücrede ilgili adımın medyan süresi (`ms`) ve adım belirteci birlikte gösterilir.  
> **Belirteç Anahtarı:** `[performans]` = Saf Rust Tavanı (`03_performans_p1c`) · `[D1:SWAR]` = `performans-swar` · `[D2:SIMD]` = `performans-simd` · `[D3:ASM]` = `performans-asm` · `[D4:Morsel]` = `performans-morsel` · `[D5:IO-mmap]` = `performans-io-mmap` · `[D6:PGO]` = `performans-pgo` · `[D7:Hibrit]` = `performans-hibrit` · `[P3:perf-1]` = `performans-1` · `[C-O3]` / `[C-O2]` = C SQLite `3.54.1`

### A. `speedbench` İş Yükleri (`W01`–`W14`) — Adım Bazlı Süreler ve En Hızlı Uygulama

| İş Yükü | `performans` Bazı (`ms`) | `D1:SWAR` (`ms`) | `D2:SIMD` (`ms`) | `D3:ASM` (`ms`) | `D4:Morsel` (`ms`) | `D5:IO-mmap` (`ms`) | `D6:PGO` (`ms`) | `D7:Hibrit` (`ms`) | `P3:perf-1` (`ms`) | En Hızlı Rust Adımı (`ms`) | C `-O3` (`ms`) | En Hızlı Uygulama (C Dahil) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `3.95 ms` `[performans]` | `4.25 ms` `[D1:SWAR]` | `3.80 ms` `[D2:SIMD]` | `3.97 ms` `[D3:ASM]` | `3.87 ms` `[D4:Morsel]` | `3.13 ms` `[D5:IO-mmap]` | `3.76 ms` `[D6:PGO]` | **`2.73 ms`** `[D7:Hibrit]` | `3.18 ms` `[P3:perf-1]` | **`2.73 ms`** `[D7:Hibrit]` (`0.69x` baz) | `5.13 ms` `[C-O3]` | **`2.73 ms` `[D7:Hibrit]`** (`0.53x` C) |
| `W02_create_indexes` | `5.53 ms` `[performans]` | `4.15 ms` `[D1:SWAR]` | `4.41 ms` `[D2:SIMD]` | `4.49 ms` `[D3:ASM]` | `2.04 ms` `[D4:Morsel]` | `4.15 ms` `[D5:IO-mmap]` | `4.30 ms` `[D6:PGO]` | **`1.50 ms`** `[D7:Hibrit]` | `1.77 ms` `[P3:perf-1]` | **`1.50 ms`** `[D7:Hibrit]` (`0.27x` baz) | `4.59 ms` `[C-O3]` | **`1.50 ms` `[D7:Hibrit]`** (`0.33x` C) |
| `W03_pk_point_select` | `21.98 ms` `[performans]` | `21.47 ms` `[D1:SWAR]` | `20.65 ms` `[D2:SIMD]` | `23.71 ms` `[D3:ASM]` | `22.39 ms` `[D4:Morsel]` | `18.30 ms` `[D5:IO-mmap]` | `19.11 ms` `[D6:PGO]` | **`15.33 ms`** `[D7:Hibrit]` | `16.77 ms` `[P3:perf-1]` | **`15.33 ms`** `[D7:Hibrit]` (`0.70x` baz) | `18.24 ms` `[C-O3]` | **`15.33 ms` `[D7:Hibrit]`** (`0.84x` C) |
| `W04_secondary_idx_range` | `7.34 ms` `[performans]` | `6.92 ms` `[D1:SWAR]` | `6.87 ms` `[D2:SIMD]` | `7.80 ms` `[D3:ASM]` | `5.65 ms` `[D4:Morsel]` | `6.25 ms` `[D5:IO-mmap]` | `6.01 ms` `[D6:PGO]` | `4.15 ms` `[D7:Hibrit]` | **`3.94 ms`** `[P3:perf-1]` | **`3.94 ms`** `[P3:perf-1]` (`0.54x` baz) | `5.29 ms` `[C-O3]` | **`3.94 ms` `[P3:perf-1]`** (`0.74x` C) |
| `W05_full_scan_filter_agg` | `7.66 ms` `[performans]` | `6.81 ms` `[D1:SWAR]` | `6.86 ms` `[D2:SIMD]` | `7.44 ms` `[D3:ASM]` | `0.95 ms` `[D4:Morsel]` | `7.08 ms` `[D5:IO-mmap]` | `7.11 ms` `[D6:PGO]` | `0.97 ms` `[D7:Hibrit]` | **`0.51 ms`** `[P3:perf-1]` | **`0.51 ms`** `[P3:perf-1]` (`0.07x` baz) | `6.90 ms` `[C-O3]` | **`0.51 ms` `[P3:perf-1]`** (`0.07x` C) |
| `W06_join_group_order` | `5.16 ms` `[performans]` | `4.58 ms` `[D1:SWAR]` | `4.58 ms` `[D2:SIMD]` | `5.98 ms` `[D3:ASM]` | **`4.50 ms`** `[D4:Morsel]` | `4.76 ms` `[D5:IO-mmap]` | `4.91 ms` `[D6:PGO]` | `4.87 ms` `[D7:Hibrit]` | `5.05 ms` `[P3:perf-1]` | **`4.50 ms`** `[D4:Morsel]` (`0.87x` baz) | `13.88 ms` `[C-O3]` | **`4.50 ms` `[D4:Morsel]`** (`0.32x` C) |
| `W07_window_functions` | `22.50 ms` `[performans]` | `16.61 ms` `[D1:SWAR]` | `16.04 ms` `[D2:SIMD]` | `24.64 ms` `[D3:ASM]` | `16.26 ms` `[D4:Morsel]` | `16.83 ms` `[D5:IO-mmap]` | `15.37 ms` `[D6:PGO]` | `14.54 ms` `[D7:Hibrit]` | **`12.47 ms`** `[P3:perf-1]` | **`12.47 ms`** `[P3:perf-1]` (`0.55x` baz) | `23.62 ms` `[C-O3]` | **`12.47 ms` `[P3:perf-1]`** (`0.53x` C) |
| `W08_indexed_update_delete` | `10.12 ms` `[performans]` | `8.51 ms` `[D1:SWAR]` | `8.85 ms` `[D2:SIMD]` | `9.91 ms` `[D3:ASM]` | `7.44 ms` `[D4:Morsel]` | `7.38 ms` `[D5:IO-mmap]` | `8.64 ms` `[D6:PGO]` | **`3.93 ms`** `[D7:Hibrit]` | `5.32 ms` `[P3:perf-1]` | **`3.93 ms`** `[D7:Hibrit]` (`0.39x` baz) | `6.59 ms` `[C-O3]` | **`3.93 ms` `[D7:Hibrit]`** (`0.60x` C) |
| `W09_like_text_scan` | `5.95 ms` `[performans]` | `4.40 ms` `[D1:SWAR]` | `4.05 ms` `[D2:SIMD]` | `3.90 ms` `[D3:ASM]` | `5.48 ms` `[D4:Morsel]` | `5.29 ms` `[D5:IO-mmap]` | `5.44 ms` `[D6:PGO]` | `4.01 ms` `[D7:Hibrit]` | **`3.32 ms`** `[P3:perf-1]` | **`3.32 ms`** `[P3:perf-1]` (`0.56x` baz) | `7.59 ms` `[C-O3]` | **`3.32 ms` `[P3:perf-1]`** (`0.44x` C) |
| `W10_jsonb_extract_agg` | `13.76 ms` `[performans]` | `14.06 ms` `[D1:SWAR]` | `11.29 ms` `[D2:SIMD]` | `13.89 ms` `[D3:ASM]` | `11.49 ms` `[D4:Morsel]` | `12.20 ms` `[D5:IO-mmap]` | `10.84 ms` `[D6:PGO]` | `6.87 ms` `[D7:Hibrit]` | **`3.03 ms`** `[P3:perf-1]` | **`3.03 ms`** `[P3:perf-1]` (`0.22x` baz) | `5.26 ms` `[C-O3]` | **`3.03 ms` `[P3:perf-1]`** (`0.58x` C) |
| `W11_fts5_index_and_bm25` | `24.42 ms` `[performans]` | `22.47 ms` `[D1:SWAR]` | `19.36 ms` `[D2:SIMD]` | `27.64 ms` `[D3:ASM]` | `20.32 ms` `[D4:Morsel]` | `20.38 ms` `[D5:IO-mmap]` | **`18.66 ms`** `[D6:PGO]` | `18.66 ms` `[D7:Hibrit]` | `25.32 ms` `[P3:perf-1]` | **`18.66 ms`** `[D6:PGO]` (`0.76x` baz) | `28.68 ms` `[C-O3]` | **`18.66 ms` `[D6:PGO]`** (`0.65x` C) |
| `W12_rtree_insert_and_bbox` | `19.73 ms` `[performans]` | `17.10 ms` `[D1:SWAR]` | `15.25 ms` `[D2:SIMD]` | `18.68 ms` `[D3:ASM]` | `11.09 ms` `[D4:Morsel]` | `16.69 ms` `[D5:IO-mmap]` | `15.26 ms` `[D6:PGO]` | **`9.45 ms`** `[D7:Hibrit]` | `10.82 ms` `[P3:perf-1]` | **`9.45 ms`** `[D7:Hibrit]` (`0.48x` baz) | `11.57 ms` `[C-O3]` | **`9.45 ms` `[D7:Hibrit]`** (`0.82x` C) |
| `W13_autocommit_wal_txns` | `7.17 ms` `[performans]` | `6.29 ms` `[D1:SWAR]` | `5.66 ms` `[D2:SIMD]` | `6.71 ms` `[D3:ASM]` | `6.92 ms` `[D4:Morsel]` | `4.73 ms` `[D5:IO-mmap]` | `5.84 ms` `[D6:PGO]` | **`4.04 ms`** `[D7:Hibrit]` | `4.34 ms` `[P3:perf-1]` | **`4.04 ms`** `[D7:Hibrit]` (`0.56x` baz) | `6.93 ms` `[C-O3]` | **`4.04 ms` `[D7:Hibrit]`** (`0.58x` C) |
| `W14_cli_sql_script` | `30.76 ms` `[performans]` | `27.05 ms` `[D1:SWAR]` | `24.91 ms` `[D2:SIMD]` | `30.63 ms` `[D3:ASM]` | `25.23 ms` `[D4:Morsel]` | `25.67 ms` `[D5:IO-mmap]` | `24.93 ms` `[D6:PGO]` | **`23.80 ms`** `[D7:Hibrit]` | `25.78 ms` `[P3:perf-1]` | **`23.80 ms`** `[D7:Hibrit]` (`0.77x` baz) | `25.84 ms` `[C-O3]` | **`23.80 ms` `[D7:Hibrit]`** (`0.92x` C) |
| **Toplam (`W01`–`W14`)** | `186.03 ms` `[performans]` | `164.67 ms` `[D1:SWAR]` | `152.56 ms` `[D2:SIMD]` | `189.39 ms` `[D3:ASM]` | `143.62 ms` `[D4:Morsel]` | `152.85 ms` `[D5:IO-mmap]` | `150.20 ms` `[D6:PGO]` | **`114.86 ms`** `[D7:Hibrit]` | `121.62 ms` `[P3:perf-1]` | **`114.86 ms`** `[D7:Hibrit]` *(Sanal zarf: `107.21 ms`)* | `170.11 ms` `[C-O3]` | **`114.86 ms` `[D7:Hibrit]`** (`0.68x` C, `14/14` iş yükünde Rust lider) |

### B. `./sqltest` Uçtan Uca CLI Aileleri (`126` Örnek) — Adım Bazlı Süreler ve En Hızlı Uygulama

| CLI Komut Ailesi | `performans` Bazı (`ms`) | `D1:SWAR` (`ms`) | `D2:SIMD` (`ms`) | `D3:ASM` (`ms`) | `D4:Morsel` (`ms`) | `D5:IO-mmap` (`ms`) | `D6:PGO` (`ms`) | `D7:Hibrit` (`ms`) | `P3:perf-1` (`ms`) | En Hızlı Rust Adımı (`ms`) | C SQLite (`ms`) | En Hızlı Uygulama (C Dahil) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `ingest/cli-import` | `29.95 ms` `[performans]` | `26.65 ms` `[D1:SWAR]` | `23.59 ms` `[D2:SIMD]` | `31.78 ms` `[D3:ASM]` | `23.97 ms` `[D4:Morsel]` | `22.42 ms` `[D5:IO-mmap]` | `23.75 ms` `[D6:PGO]` | **`22.03 ms`** `[D7:Hibrit]` | `31.46 ms` `[P3:perf-1]` | **`22.03 ms`** `[D7:Hibrit]` (`0.74x` baz) | `34.00 ms` `[C-O2]` | **`22.03 ms` `[D7:Hibrit]`** (`0.65x` C) |
| `ingest/cli-persistent-auto` | `15.33 ms` `[performans]` | `13.62 ms` `[D1:SWAR]` | `12.37 ms` `[D2:SIMD]` | `15.49 ms` `[D3:ASM]` | `13.09 ms` `[D4:Morsel]` | `10.98 ms` `[D5:IO-mmap]` | `12.44 ms` `[D6:PGO]` | **`9.96 ms`** `[D7:Hibrit]` | `12.36 ms` `[P3:perf-1]` | **`9.96 ms`** `[D7:Hibrit]` (`0.65x` baz) | `15.03 ms` `[C-O2]` | **`9.96 ms` `[D7:Hibrit]`** (`0.66x` C) |
| `ingest/cli-persistent-batch` | `9.48 ms` `[performans]` | `8.40 ms` `[D1:SWAR]` | `7.87 ms` `[D2:SIMD]` | `11.26 ms` `[D3:ASM]` | `7.05 ms` `[D4:Morsel]` | `7.25 ms` `[D5:IO-mmap]` | `8.00 ms` `[D6:PGO]` | **`5.93 ms`** `[D7:Hibrit]` | `8.16 ms` `[P3:perf-1]` | **`5.93 ms`** `[D7:Hibrit]` (`0.63x` baz) | `21.76 ms` `[C-O2]` | **`5.93 ms` `[D7:Hibrit]`** (`0.27x` C) |
| `ingest/cli-read-auto` | `33.34 ms` `[performans]` | `30.76 ms` `[D1:SWAR]` | `27.62 ms` `[D2:SIMD]` | `36.23 ms` `[D3:ASM]` | `27.72 ms` `[D4:Morsel]` | `25.69 ms` `[D5:IO-mmap]` | `27.00 ms` `[D6:PGO]` | **`25.20 ms`** `[D7:Hibrit]` | `31.43 ms` `[P3:perf-1]` | **`25.20 ms`** `[D7:Hibrit]` (`0.76x` baz) | `33.86 ms` `[C-O2]` | **`25.20 ms` `[D7:Hibrit]`** (`0.74x` C) |
| `ingest/cli-read-batch` | `30.78 ms` `[performans]` | `28.51 ms` `[D1:SWAR]` | `25.33 ms` `[D2:SIMD]` | `34.18 ms` `[D3:ASM]` | `23.56 ms` `[D4:Morsel]` | `23.26 ms` `[D5:IO-mmap]` | `24.80 ms` `[D6:PGO]` | **`22.93 ms`** `[D7:Hibrit]` | `28.80 ms` `[P3:perf-1]` | **`22.93 ms`** `[D7:Hibrit]` (`0.74x` baz) | `44.45 ms` `[C-O2]` | **`22.93 ms` `[D7:Hibrit]`** (`0.52x` C) |
| `uuid-query/join-index` | `15.85 ms` `[performans]` | `13.08 ms` `[D1:SWAR]` | **`11.10 ms`** `[D2:SIMD]` | `15.47 ms` `[D3:ASM]` | `11.80 ms` `[D4:Morsel]` | `11.27 ms` `[D5:IO-mmap]` | `11.49 ms` `[D6:PGO]` | `12.19 ms` `[D7:Hibrit]` | `18.42 ms` `[P3:perf-1]` | **`11.10 ms`** `[D2:SIMD]` (`0.70x` baz) | `32.12 ms` `[C-O2]` | **`11.10 ms` `[D2:SIMD]`** (`0.35x` C) |
| `uuid-query/join-noindex` | `12.52 ms` `[performans]` | `11.63 ms` `[D1:SWAR]` | **`10.23 ms`** `[D2:SIMD]` | `14.57 ms` `[D3:ASM]` | `10.89 ms` `[D4:Morsel]` | `11.04 ms` `[D5:IO-mmap]` | `10.96 ms` `[D6:PGO]` | `11.14 ms` `[D7:Hibrit]` | `16.51 ms` `[P3:perf-1]` | **`10.23 ms`** `[D2:SIMD]` (`0.82x` baz) | `27.84 ms` `[C-O2]` | **`10.23 ms` `[D2:SIMD]`** (`0.37x` C) |
| **Geomean (`Rust / C`)** | `0.668x` `[performans]` | `0.598x` `[D1:SWAR]` | `0.533x` `[D2:SIMD]` | `0.723x` `[D3:ASM]` | `0.534x` `[D4:Morsel]` | `0.509x` `[D5:IO-mmap]` | `0.540x` `[D6:PGO]` | **`0.490x`** `[D7:Hibrit]` | `0.666x` `[P3:perf-1]` | **`0.490x`** `[D7:Hibrit]` | `1.000x` `[C-O2]` | **`0.490x` `[D7:Hibrit]`** (`7/7` CLI ailesinde Rust lider) |

### C. Adım Adım Kümülatif En Hızlı Uygulama Evrimi (`speedbench` `W01`–`W14`)

> Her sütun, o adıma kadar ulaşılan **en hızlı süreyi** ve bu rekorun **hangi adıma ait olduğunu (`[Belirteç]`)** gösterir.

| İş Yükü | `performans` Dalı (`ms`) | Adım 1 (`D1:SWAR` Sonrası En Hızlı) | Adım 2 (`D2:SIMD` Sonrası En Hızlı) | Adım 3 (`D3:ASM` Sonrası En Hızlı) | Adım 4 (`D4:Morsel` Sonrası En Hızlı) | Adım 5 (`D5:IO-mmap` Sonrası En Hızlı) | Adım 6 (`D6:PGO` Sonrası En Hızlı) | Adım 7 (`D7:Hibrit` Sonrası En Hızlı) | Adım 8 (`P3:perf-1` Sonrası En Hızlı) | C `-O3` (`ms`) | Güncel Mutlak En Hızlı (C Dahil) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `3.95 ms` `[performans]` | `3.95 ms` `[performans]` | **`3.80 ms`** `[D2:SIMD]` | `3.80 ms` `[D2:SIMD]` | `3.80 ms` `[D2:SIMD]` | **`3.13 ms`** `[D5:IO-mmap]` | `3.13 ms` `[D5:IO-mmap]` | **`2.73 ms`** `[D7:Hibrit]` | `2.73 ms` `[D7:Hibrit]` | `5.13 ms` `[C-O3]` | **`2.73 ms` `[D7:Hibrit]`** (`0.53x` C) |
| `W02_create_indexes` | `5.53 ms` `[performans]` | **`4.15 ms`** `[D1:SWAR]` | `4.15 ms` `[D1:SWAR]` | `4.15 ms` `[D1:SWAR]` | **`2.04 ms`** `[D4:Morsel]` | `2.04 ms` `[D4:Morsel]` | `2.04 ms` `[D4:Morsel]` | **`1.50 ms`** `[D7:Hibrit]` | `1.50 ms` `[D7:Hibrit]` | `4.59 ms` `[C-O3]` | **`1.50 ms` `[D7:Hibrit]`** (`0.33x` C) |
| `W03_pk_point_select` | `21.98 ms` `[performans]` | **`21.47 ms`** `[D1:SWAR]` | **`20.65 ms`** `[D2:SIMD]` | `20.65 ms` `[D2:SIMD]` | `20.65 ms` `[D2:SIMD]` | **`18.30 ms`** `[D5:IO-mmap]` | `18.30 ms` `[D5:IO-mmap]` | **`15.33 ms`** `[D7:Hibrit]` | `15.33 ms` `[D7:Hibrit]` | `18.24 ms` `[C-O3]` | **`15.33 ms` `[D7:Hibrit]`** (`0.84x` C) |
| `W04_secondary_idx_range` | `7.34 ms` `[performans]` | **`6.92 ms`** `[D1:SWAR]` | **`6.87 ms`** `[D2:SIMD]` | `6.87 ms` `[D2:SIMD]` | **`5.65 ms`** `[D4:Morsel]` | `5.65 ms` `[D4:Morsel]` | `5.65 ms` `[D4:Morsel]` | **`4.15 ms`** `[D7:Hibrit]` | **`3.94 ms`** `[P3:perf-1]` | `5.29 ms` `[C-O3]` | **`3.94 ms` `[P3:perf-1]`** (`0.74x` C) |
| `W05_full_scan_filter_agg` | `7.66 ms` `[performans]` | **`6.81 ms`** `[D1:SWAR]` | `6.81 ms` `[D1:SWAR]` | `6.81 ms` `[D1:SWAR]` | **`0.95 ms`** `[D4:Morsel]` | `0.95 ms` `[D4:Morsel]` | `0.95 ms` `[D4:Morsel]` | `0.95 ms` `[D4:Morsel]` | **`0.51 ms`** `[P3:perf-1]` | `6.90 ms` `[C-O3]` | **`0.51 ms` `[P3:perf-1]`** (`0.07x` C) |
| `W06_join_group_order` | `5.16 ms` `[performans]` | **`4.58 ms`** `[D1:SWAR]` | **`4.58 ms`** `[D2:SIMD]` | `4.58 ms` `[D2:SIMD]` | **`4.50 ms`** `[D4:Morsel]` | `4.50 ms` `[D4:Morsel]` | `4.50 ms` `[D4:Morsel]` | `4.50 ms` `[D4:Morsel]` | `4.50 ms` `[D4:Morsel]` | `13.88 ms` `[C-O3]` | **`4.50 ms` `[D4:Morsel]`** (`0.32x` C) |
| `W07_window_functions` | `22.50 ms` `[performans]` | **`16.61 ms`** `[D1:SWAR]` | **`16.04 ms`** `[D2:SIMD]` | `16.04 ms` `[D2:SIMD]` | `16.04 ms` `[D2:SIMD]` | `16.04 ms` `[D2:SIMD]` | **`15.37 ms`** `[D6:PGO]` | **`14.54 ms`** `[D7:Hibrit]` | **`12.47 ms`** `[P3:perf-1]` | `23.62 ms` `[C-O3]` | **`12.47 ms` `[P3:perf-1]`** (`0.53x` C) |
| `W08_indexed_update_delete` | `10.12 ms` `[performans]` | **`8.51 ms`** `[D1:SWAR]` | `8.51 ms` `[D1:SWAR]` | `8.51 ms` `[D1:SWAR]` | **`7.44 ms`** `[D4:Morsel]` | **`7.38 ms`** `[D5:IO-mmap]` | `7.38 ms` `[D5:IO-mmap]` | **`3.93 ms`** `[D7:Hibrit]` | `3.93 ms` `[D7:Hibrit]` | `6.59 ms` `[C-O3]` | **`3.93 ms` `[D7:Hibrit]`** (`0.60x` C) |
| `W09_like_text_scan` | `5.95 ms` `[performans]` | **`4.40 ms`** `[D1:SWAR]` | **`4.05 ms`** `[D2:SIMD]` | **`3.90 ms`** `[D3:ASM]` | `3.90 ms` `[D3:ASM]` | `3.90 ms` `[D3:ASM]` | `3.90 ms` `[D3:ASM]` | `3.90 ms` `[D3:ASM]` | **`3.32 ms`** `[P3:perf-1]` | `7.59 ms` `[C-O3]` | **`3.32 ms` `[P3:perf-1]`** (`0.44x` C) |
| `W10_jsonb_extract_agg` | `13.76 ms` `[performans]` | `13.76 ms` `[performans]` | **`11.29 ms`** `[D2:SIMD]` | `11.29 ms` `[D2:SIMD]` | `11.29 ms` `[D2:SIMD]` | `11.29 ms` `[D2:SIMD]` | **`10.84 ms`** `[D6:PGO]` | **`6.87 ms`** `[D7:Hibrit]` | **`3.03 ms`** `[P3:perf-1]` | `5.26 ms` `[C-O3]` | **`3.03 ms` `[P3:perf-1]`** (`0.58x` C) |
| `W11_fts5_index_and_bm25` | `24.42 ms` `[performans]` | **`22.47 ms`** `[D1:SWAR]` | **`19.36 ms`** `[D2:SIMD]` | `19.36 ms` `[D2:SIMD]` | `19.36 ms` `[D2:SIMD]` | `19.36 ms` `[D2:SIMD]` | **`18.66 ms`** `[D6:PGO]` | `18.66 ms` `[D6:PGO]` | `18.66 ms` `[D6:PGO]` | `28.68 ms` `[C-O3]` | **`18.66 ms` `[D6:PGO]`** (`0.65x` C) |
| `W12_rtree_insert_and_bbox` | `19.73 ms` `[performans]` | **`17.10 ms`** `[D1:SWAR]` | **`15.25 ms`** `[D2:SIMD]` | `15.25 ms` `[D2:SIMD]` | **`11.09 ms`** `[D4:Morsel]` | `11.09 ms` `[D4:Morsel]` | `11.09 ms` `[D4:Morsel]` | **`9.45 ms`** `[D7:Hibrit]` | `9.45 ms` `[D7:Hibrit]` | `11.57 ms` `[C-O3]` | **`9.45 ms` `[D7:Hibrit]`** (`0.82x` C) |
| `W13_autocommit_wal_txns` | `7.17 ms` `[performans]` | **`6.29 ms`** `[D1:SWAR]` | **`5.66 ms`** `[D2:SIMD]` | `5.66 ms` `[D2:SIMD]` | `5.66 ms` `[D2:SIMD]` | **`4.73 ms`** `[D5:IO-mmap]` | `4.73 ms` `[D5:IO-mmap]` | **`4.04 ms`** `[D7:Hibrit]` | `4.04 ms` `[D7:Hibrit]` | `6.93 ms` `[C-O3]` | **`4.04 ms` `[D7:Hibrit]`** (`0.58x` C) |
| `W14_cli_sql_script` | `30.76 ms` `[performans]` | **`27.05 ms`** `[D1:SWAR]` | **`24.91 ms`** `[D2:SIMD]` | `24.91 ms` `[D2:SIMD]` | `24.91 ms` `[D2:SIMD]` | `24.91 ms` `[D2:SIMD]` | `24.91 ms` `[D2:SIMD]` | **`23.80 ms`** `[D7:Hibrit]` | `23.80 ms` `[D7:Hibrit]` | `25.84 ms` `[C-O3]` | **`23.80 ms` `[D7:Hibrit]`** (`0.92x` C) |
| **En Hızlılar Toplamı (`W01`–`W14`)** | `186.03 ms` `[performans]` | **`164.08 ms`** `[<= D1:SWAR]` | **`151.92 ms`** `[<= D2:SIMD]` | **`151.77 ms`** `[<= D3:ASM]` | **`137.26 ms`** `[<= D4:Morsel]` | **`133.26 ms`** `[<= D5:IO-mmap]` | **`131.45 ms`** `[<= D6:PGO]` | **`114.35 ms`** `[<= D7:Hibrit]` | **`107.21 ms`** `[<= P3:perf-1]` | `170.11 ms` `[C-O3]` | **`107.21 ms` `[Birleşik En Hızlı Rust Zarfı]`** (`0.63x` C) |

### D. Adım Adım Kümülatif En Hızlı Uygulama Evrimi (`./sqltest` `126` Örnek CLI)

| CLI Komut Ailesi | `performans` Dalı (`ms`) | Adım 1 (`D1:SWAR` Sonrası En Hızlı) | Adım 2 (`D2:SIMD` Sonrası En Hızlı) | Adım 3 (`D3:ASM` Sonrası En Hızlı) | Adım 4 (`D4:Morsel` Sonrası En Hızlı) | Adım 5 (`D5:IO-mmap` Sonrası En Hızlı) | Adım 6 (`D6:PGO` Sonrası En Hızlı) | Adım 7 (`D7:Hibrit` Sonrası En Hızlı) | Adım 8 (`P3:perf-1` Sonrası En Hızlı) | C SQLite (`ms`) | Güncel Mutlak En Hızlı (C Dahil) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `ingest/cli-import` | `29.95 ms` `[performans]` | **`26.65 ms`** `[D1:SWAR]` | **`23.59 ms`** `[D2:SIMD]` | `23.59 ms` `[D2:SIMD]` | `23.59 ms` `[D2:SIMD]` | **`22.42 ms`** `[D5:IO-mmap]` | `22.42 ms` `[D5:IO-mmap]` | **`22.03 ms`** `[D7:Hibrit]` | `22.03 ms` `[D7:Hibrit]` | `34.00 ms` `[C-O2]` | **`22.03 ms` `[D7:Hibrit]`** (`0.65x` C) |
| `ingest/cli-persistent-auto` | `15.33 ms` `[performans]` | **`13.62 ms`** `[D1:SWAR]` | **`12.37 ms`** `[D2:SIMD]` | `12.37 ms` `[D2:SIMD]` | `12.37 ms` `[D2:SIMD]` | **`10.98 ms`** `[D5:IO-mmap]` | `10.98 ms` `[D5:IO-mmap]` | **`9.96 ms`** `[D7:Hibrit]` | `9.96 ms` `[D7:Hibrit]` | `15.03 ms` `[C-O2]` | **`9.96 ms` `[D7:Hibrit]`** (`0.66x` C) |
| `ingest/cli-persistent-batch` | `9.48 ms` `[performans]` | **`8.40 ms`** `[D1:SWAR]` | **`7.87 ms`** `[D2:SIMD]` | `7.87 ms` `[D2:SIMD]` | **`7.05 ms`** `[D4:Morsel]` | `7.05 ms` `[D4:Morsel]` | `7.05 ms` `[D4:Morsel]` | **`5.93 ms`** `[D7:Hibrit]` | `5.93 ms` `[D7:Hibrit]` | `21.76 ms` `[C-O2]` | **`5.93 ms` `[D7:Hibrit]`** (`0.27x` C) |
| `ingest/cli-read-auto` | `33.34 ms` `[performans]` | **`30.76 ms`** `[D1:SWAR]` | **`27.62 ms`** `[D2:SIMD]` | `27.62 ms` `[D2:SIMD]` | `27.62 ms` `[D2:SIMD]` | **`25.69 ms`** `[D5:IO-mmap]` | `25.69 ms` `[D5:IO-mmap]` | **`25.20 ms`** `[D7:Hibrit]` | `25.20 ms` `[D7:Hibrit]` | `33.86 ms` `[C-O2]` | **`25.20 ms` `[D7:Hibrit]`** (`0.74x` C) |
| `ingest/cli-read-batch` | `30.78 ms` `[performans]` | **`28.51 ms`** `[D1:SWAR]` | **`25.33 ms`** `[D2:SIMD]` | `25.33 ms` `[D2:SIMD]` | **`23.56 ms`** `[D4:Morsel]` | **`23.26 ms`** `[D5:IO-mmap]` | `23.26 ms` `[D5:IO-mmap]` | **`22.93 ms`** `[D7:Hibrit]` | `22.93 ms` `[D7:Hibrit]` | `44.45 ms` `[C-O2]` | **`22.93 ms` `[D7:Hibrit]`** (`0.52x` C) |
| `uuid-query/join-index` | `15.85 ms` `[performans]` | **`13.08 ms`** `[D1:SWAR]` | **`11.10 ms`** `[D2:SIMD]` | `11.10 ms` `[D2:SIMD]` | `11.10 ms` `[D2:SIMD]` | `11.10 ms` `[D2:SIMD]` | `11.10 ms` `[D2:SIMD]` | `11.10 ms` `[D2:SIMD]` | `11.10 ms` `[D2:SIMD]` | `32.12 ms` `[C-O2]` | **`11.10 ms` `[D2:SIMD]`** (`0.35x` C) |
| `uuid-query/join-noindex` | `12.52 ms` `[performans]` | **`11.63 ms`** `[D1:SWAR]` | **`10.23 ms`** `[D2:SIMD]` | `10.23 ms` `[D2:SIMD]` | `10.23 ms` `[D2:SIMD]` | `10.23 ms` `[D2:SIMD]` | `10.23 ms` `[D2:SIMD]` | `10.23 ms` `[D2:SIMD]` | `10.23 ms` `[D2:SIMD]` | `27.84 ms` `[C-O2]` | **`10.23 ms` `[D2:SIMD]`** (`0.37x` C) |
