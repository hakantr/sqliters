# `performans-A-hizalama` — C'ye Hizalama Adımları (Bedelli) Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-14T01:26:31+03:00`
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
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`294.16`** | `—` | **`1.000x`** | **`1.000x`** | `6,164` | `—` |
| **00_performans_A_ucu** | `performans-A@225f5c2` | `7,299.0` | `6,157.8` | **`247.49`** | `1.000x` | **`0.821x`** | `0.841x` | `18,172` | 14/14 ✓ |
| **01_h1_silmede_dengeleme** | `performans-A-hizalama@034af4f` | `7,346.9` | `6,193.5` | **`247.41`** | `0.999x` | **`0.821x`** | `0.841x` | `18,292` | 14/14 ✓ |
| **02_h1b_dml_stratejisi** | `performans-A-hizalama@ebc3fde` | `7,461.8` | `6,294.8` | **`249.33`** | `1.005x` | **`0.825x`** | `0.848x` | `18,576` | 14/14 ✓ |
| **03_h2_eklemede_dengeleme** | `performans-A-hizalama@0c65dbe` | `7,376.3` | `6,223.2` | **`249.65`** | `1.004x` | **`0.825x`** | `0.849x` | `18,160` | 14/14 ✓ |
| **04_h3_toplu_kurma** | `performans-A-hizalama@fd7364d` | `7,363.6` | `6,217.5` | **`246.82`** | `0.984x` | **`0.808x`** | `0.839x` | `18,716` | 14/14 ✓ |
| **05_h4_rtree** | `performans-A-hizalama@b734244` | `7,420.3` | `6,267.2` | **`252.87`** | `1.004x` | **`0.825x`** | `0.860x` | `18,688` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_A_ucu | 01_h1_silmede_dengeleme | 02_h1b_dml_stratejisi | 03_h2_eklemede_dengeleme | 04_h3_toplu_kurma | 05_h4_rtree |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.65` | `7.52` (**0.78x**) | `7.48` (**0.77x**) | `7.43` (**0.77x**) | `7.61` (**0.79x**) | `7.89` (**0.82x**) | `8.06` (**0.83x**) |
| `W02_create_indexes` | `8.68` | `7.47` (**0.86x**) | `7.50` (**0.86x**) | `7.55` (**0.87x**) | `7.51` (**0.86x**) | `5.71` (**0.66x**) | `5.68` (**0.66x**) |
| `W03_pk_point_select` | `23.26` | `27.69` (**1.19x**) | `27.86` (**1.20x**) | `27.98` (**1.20x**) | `27.86` (**1.20x**) | `28.38` (**1.22x**) | `28.05` (**1.21x**) |
| `W04_secondary_idx_range` | `7.43` | `7.75` (**1.04x**) | `7.81` (**1.05x**) | `7.79` (**1.05x**) | `7.88` (**1.06x**) | `7.80` (**1.05x**) | `7.81` (**1.05x**) |
| `W05_full_scan_filter_agg` | `16.62` | `4.28` (**0.26x**) | `4.36` (**0.26x**) | `4.27` (**0.26x**) | `4.28` (**0.26x**) | `4.06` (**0.24x**) | `4.10` (**0.25x**) |
| `W06_join_group_order` | `28.29` | `10.75` (**0.38x**) | `10.63` (**0.38x**) | `11.01` (**0.39x**) | `10.73` (**0.38x**) | `10.93` (**0.39x**) | `10.69` (**0.38x**) |
| `W07_window_functions` | `50.16` | `38.68` (**0.77x**) | `38.26` (**0.76x**) | `38.77` (**0.77x**) | `38.97` (**0.78x**) | `37.83` (**0.75x**) | `37.64` (**0.75x**) |
| `W08_indexed_update_delete` | `8.00` | `9.60` (**1.20x**) | `9.84` (**1.23x**) | `9.80` (**1.23x**) | `9.46` (**1.18x**) | `9.74` (**1.22x**) | `9.89` (**1.24x**) |
| `W09_like_text_scan` | `18.96` | `11.99` (**0.63x**) | `11.88` (**0.63x**) | `11.89` (**0.63x**) | `11.99` (**0.63x**) | `11.98` (**0.63x**) | `12.06` (**0.64x**) |
| `W10_jsonb_extract_agg` | `10.89` | `11.33` (**1.04x**) | `11.12` (**1.02x**) | `11.20` (**1.03x**) | `11.18` (**1.03x**) | `11.06` (**1.02x**) | `11.35` (**1.04x**) |
| `W11_fts5_index_and_bm25` | `57.23` | `45.27` (**0.79x**) | `45.63` (**0.80x**) | `45.94` (**0.80x**) | `46.72` (**0.82x**) | `45.83` (**0.80x**) | `42.88` (**0.75x**) |
| `W12_rtree_insert_and_bbox` | `24.35` | `23.73` (**0.97x**) | `23.87` (**0.98x**) | `24.14` (**0.99x**) | `24.18` (**0.99x**) | `23.72` (**0.97x**) | `33.10` (**1.36x**) |
| `W13_autocommit_wal_txns` | `4.04` | `4.90` (**1.22x**) | `4.72` (**1.17x**) | `4.87` (**1.21x**) | `4.83` (**1.20x**) | `4.94` (**1.22x**) | `4.90` (**1.22x**) |
| `W14_cli_sql_script` | `26.60` | `36.55` (**1.37x**) | `36.44` (**1.37x**) | `36.69` (**1.38x**) | `36.46` (**1.37x**) | `36.94` (**1.39x**) | `36.64` (**1.38x**) |
| **Toplam** | **`294.16`** | **`247.49`** (**0.84x**) | **`247.41`** (**0.84x**) | **`249.33`** (**0.85x**) | **`249.65`** (**0.85x**) | **`246.82`** (**0.84x**) | **`252.87`** (**0.86x**) |

---

## 00_performans_A_ucu — `performans-A@225f5c2`

**Baz.** `performans-A` dalının bu dal ayrılırken ucu (adım 5j). Ayrışma bulucu, 234 senaryo, son dosya katmanı: L0 56, L2 43, L3 3, L4 58, L5 59, L6 15.

- **Testler**: `cargo test --workspace`: 757 geçti, bilinen 4 başarısız, 337 yok sayıldı.
- **Toplam**: `247.49 ms` (taban `247.49 ms`, C `294.16 ms`) — geomean `Rust/C` **`0.821x`** (disk WAL `0.794x`, bellek-içi `0.929x`)
- **Tepe RSS**: `18,172 KiB` (C: `6,164 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.52`** | `7.50` | `1.000x` | `9.65` | **`0.78x`** | `9,160` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.47`** | `7.42` | `1.000x` | `8.68` | **`0.86x`** | `10,000` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.69`** | `27.48` | `1.000x` | `23.26` | **`1.19x`** | `10,000` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.75`** | `7.69` | `1.000x` | `7.43` | **`1.04x`** | `10,128` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `1.000x` | `16.62` | **`0.26x`** | `10,128` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.75`** | `10.70` | `1.000x` | `28.29` | **`0.38x`** | `10,192` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.68`** | `38.38` | `1.000x` | `50.16` | **`0.77x`** | `12,616` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.60`** | `9.47` | `1.000x` | `8.00` | **`1.20x`** | `12,616` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.99`** | `11.82` | `1.000x` | `18.96` | **`0.63x`** | `12,616` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.33`** | `11.21` | `1.000x` | `10.89` | **`1.04x`** | `12,864` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.27`** | `44.82` | `1.000x` | `57.23` | **`0.79x`** | `17,972` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.73`** | `23.25` | `1.000x` | `24.35` | **`0.97x`** | `18,044` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.90`** | `4.48` | `1.000x` | `4.04` | **`1.22x`** | `18,076` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.55`** | `36.26` | `1.000x` | `26.60` | **`1.37x`** | `18,044` | `fc7490e2b80c68e2` ✓ |

---

## 01_h1_silmede_dengeleme — `performans-A-hizalama@034af4f`

**H1 — silmede C gibi yeniden dengeleme.** C, bir silmeden sonra sayfanın boş alanı kullanılabilir alanın üçte ikisini aşarsa `balance()` çağırır ve `balance_nonroot()` hücreleri en çok üç kardeş arasında yeniden dağıtır (gerekirse bir sayfayı serbest bırakır, kökte bir seviye düşürür); iç sayfadaki bir hücre silinince yerine öncül yaprak hücresi konur. Motor yalnızca yaprak tamamen boşaldığında davranıyordu; iç indeks hücresi silmede bazı durumlarda tüm indeksi yeniden kuruyordu. Yeni `sqliters-btree/src/balance.rs`, C'nin `balance` / `balance_nonroot` / `balance_deeper` / `balance_quick`, `insertCell` / `dropCell` / `allocateSpace` / `defragmentPage`, `editPage` / `rebuildPage` işlevlerinin portudur; tüm silme yolları (genel, konumlu, ham hızlı yollar, indeks) buna bağlandı. Hızlı yollar ham kaldı: üçte iki testi O(1) bir sınırla başlıyor. Ekleme tarafı bu adımda değişmedi (H2 aynı kodu etkinleştirecek). Motor düzeyinde bir düzeltme de gerekti: satır kimliği çakışmasında `REPLACE` artık C gibi yalnızca indeks girdilerini silip satırın üzerine yazıyor.

- **Testler**: `cargo test --workspace`: 764 geçti (7 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Karar eşliği** (iki motor aynı başlangıç dosyasından; 95 senaryo, 762.748 C olayı): 93 senaryoda her karar, SQL çıktısı, bütünlük ve sayfa sayfa şekil C ile aynı; kalan ikisi bu adımdan önce de var olan motor düzeyi farklar (indeks taramalı silmede ağaçlar arası yazma sırası; tetikleyici içi `DELETE`in satır ziyaret sırası). Hiç tetiklenmeyen kararlar: `balance_quick` ve `bBulk` dalları (yalnızca eklemede), bozuk girdi dalları. Ayrıntı: `karar_esligi_h1.md`. **Ayrışma bulucu** (245 senaryo, son derleme): kötüleşen yok, 24 iyileşti (L5 59 → 39, L2 44 → 55). **Sayfa sayıları (motor/C):** karışık 4096: 296 → 251 / 249; 512: 2456 → 2214 / 2173; 1024 + auto_vacuum: 1182 → 986 / 972; WAL: 281 → 244 / 244. Kalan fark ekleme tarafı bölünmelerden (H2). **Farksal koşu:** 4.800 tohum, çıktı C ile ve dal noktasıyla aynı, her dosya C `integrity_check` geçiyor. **Çökme/geri alma:** 288 betik + commit sırasında öldürülen 2.660 kurtarma, 0 hata. **Kurtarma eşliği (kapı tam karşılanmadı):** otomatik commit'te aynı şekilli 93 dosyanın 93'ünde C `.recover` çıktısı aynı; açık işlem içinde 99'un 84'ünde. Neden bu adımdan önce de var: motor açık işlemdeki her genel-yol ifadesi için ifade kayıt noktası açıyor (C yalnızca çok satırlı ve iptal olabilecek ifadelerde), bu yüzden serbest kalan sayfanın yazımı düşürülemiyor. **Bedel:** `W08` süre +%5,0 (ajanın koşusunda +%3,3), komut +%0,75; diğer iş yüklerinde komut sayısı aynı. Kalan maliyet: C sayfa başına boş alanı önbellekte tutuyor (`nFree`), port gerektiğinde yeniden hesaplıyor.
- **Toplam**: `247.41 ms` (taban `247.49 ms`, C `294.16 ms`) — geomean `Rust/C` **`0.821x`** (disk WAL `0.794x`, bellek-içi `0.927x`)
- **Tepe RSS**: `18,292 KiB` (C: `6,164 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.48`** | `7.45` | `7.52` | **`0.994x`** | `0.994x` | `9.65` | **`0.77x`** | `9,088` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.50`** | `7.42` | `7.47` | **`1.005x`** | `1.005x` | `8.68` | **`0.86x`** | `9,924` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.86`** | `27.67` | `27.69` | **`1.006x`** | `1.006x` | `23.26` | **`1.20x`** | `9,924` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.81`** | `7.74` | `7.75` | **`1.008x`** | `1.008x` | `7.43` | **`1.05x`** | `9,988` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.36`** | `4.36` | `4.28` | **`1.021x`** | `1.021x` | `16.62` | **`0.26x`** | `9,988` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.60` | `10.75` | **`0.989x`** | `0.989x` | `28.29` | **`0.38x`** | `10,116` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.26`** | `38.17` | `38.68` | **`0.989x`** | `0.989x` | `50.16` | **`0.76x`** | `12,544` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.84`** | `9.76` | `9.60` | **`1.025x`** | `1.025x` | `8.00` | **`1.23x`** | `12,608` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.88`** | `11.86` | `11.99` | **`0.990x`** | `0.990x` | `18.96` | **`0.63x`** | `12,608` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.12`** | `11.08` | `11.33` | **`0.982x`** | `0.982x` | `10.89` | **`1.02x`** | `12,788` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.63`** | `45.00` | `45.27` | **`1.008x`** | `1.008x` | `57.23` | **`0.80x`** | `18,016` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.87`** | `23.25` | `23.73` | **`1.006x`** | `1.006x` | `24.35` | **`0.98x`** | `18,064` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.72`** | `4.54` | `4.90` | **`0.964x`** | `0.964x` | `4.04` | **`1.17x`** | `18,096` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.44`** | `35.97` | `36.55` | **`0.997x`** | `0.997x` | `26.60` | **`1.37x`** | `18,064` | `fc7490e2b80c68e2` ✓ |

---

## 02_h1b_dml_stratejisi — `performans-A-hizalama@ebc3fde`

**H1b — DML ifadelerinin C'nin kod üreticisi gibi yürütülmesi.** (A) **Geçiş stratejisi ve satır ziyaret sırası**: `DELETE` ve `UPDATE` için C'nin kararları port edildi (tek geçiş / satır kümesi / geçici indeks; `bComplex`, `chngKey`, `hasFK`, kısaltma optimizasyonu koşulları); iki geçişte satırlar C gibi satır kimliği/birincil anahtar sırasıyla işleniyor. Tetikleyicili tabloda indeks üzerinden `DELETE` artık C ile aynı sonucu veriyor (önce sonuç farklıydı). `AUTOINCREMENT` ifade başına sayaç kullanıyor; `last_insert_rowid` ve `total_changes` başarısız ifadede C gibi geri alınmıyor; tek satırlık `INSERT`te eksik üst satır hemen hata veriyor. (B) **Kısıt kontrol sırası ve yazma sırası**: NOT NULL → CHECK → satır kimliği → UNIQUE indeksler (C'nin indeks sırasıyla; önce ters sıradaydı); `STRICT` tür kontrolü C'nin konumunda; `REPLACE` çakışan indeks girdisini en son siliyor; üretilmiş sütunlar bağımlılık sırasıyla hesaplanıyor (ileri başvuruda yanlış indeks girdisi yazılıyor ve C `integrity_check` düşüyordu). (C) **İfade günlüğü** yalnızca C'nin açtığı durumda açılıyor (`isMultiWrite && mayAbort`); başarısız ifadenin neyi geri aldığı `sqlite3VdbeHalt` kurallarına bağlandı. Bir entegrasyon testi beklentisi C'ye göre değişti (başarısız commit'ten sonra `last_insert_rowid`).

- **Testler**: `cargo test --workspace`: 781 geçti (17 yeni), bilinen 4 başarısız, 337 yok sayıldı. **DML farksalı** (rastgele şema, tetikleyici, yabancı anahtar, çakışma yan tümceleri; CLI + API; her ifadeden sonra `changes`/`total_changes`/`last_insert_rowid`): ajanın 3.000 tohumunda C ile birebir aynı 2.839; bağımsız 400 yeni tohumda (yalnız CLI) **377** — H1 ve dal noktası 134. Dal noktasında görülen 2 C `integrity_check` hatası kalmadı. Kalan farklar (örnekle sınıflandı): SELECT planlayıcısından gelen satır sırası (görünümler, WITHOUT ROWID indeks seçimi), DDL hata metinleri, iki yabancı anahtar ayrıntısı, `INSERT…SELECT`in eş-yordam durumu. **Karar izi** (600 tohum, 6.487 ifade): günlük açma 0 uyuşmazlık, durdurma sonucu 0, strateji 28, günlük bayrakları 9; B-tree yazma dizisi 6.184 eşit / 303 farklı. **Kurtarma eşliği:** açık işlem içinde 99/100 aynı (H1: 84/99); kalan durum serbest sayfa yolu farkı. **H1 karar izi:** 95/95 tam eşit (H1: 93). **Ayrışma bulucu:** dağılım aynı, kötüleşen yok. **Çökme/geri alma:** yeni ifade-iptali takımı 84 betik + 432 kurtarma, diğer iki takım hızlı kipte, 0 hata. **Bedel:** toplam 1,007x; komut sayıları 0,999–1,001, yalnız `W12` +%0,9 ve `W11` +%0,3 (ifade başına günlük politikası çağrısı). `W06` +%3,8 süre aynı komut sayısıyla (kod yerleşimi). **Hizalanmayan:** WITHOUT ROWID `UPDATE` tüm indeks girdilerini siliyor/yeniden ekliyor (C yalnız değişenleri); UPSERT'te silme sırası; C'nin eş-yordamlı `INSERT…SELECT`i; DDL/ANALYZE/PRAGMA ifade günlüğü bayrakları; 9 bayrak uyuşmazlığının ~5'inde C'nin günlük açtığı yerde motor açmıyor (farksalda gözlenmedi, dışlanmadı).
- **Toplam**: `249.33 ms` (taban `247.49 ms`, C `294.16 ms`) — geomean `Rust/C` **`0.825x`** (disk WAL `0.798x`, bellek-içi `0.935x`)
- **Tepe RSS**: `18,576 KiB` (C: `6,164 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.43`** | `7.42` | `7.48` | **`0.994x`** | `0.988x` | `9.65` | **`0.77x`** | `9,148` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.55`** | `7.46` | `7.50` | **`1.006x`** | `1.011x` | `8.68` | **`0.87x`** | `9,980` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.98`** | `27.50` | `27.86` | **`1.004x`** | `1.011x` | `23.26` | **`1.20x`** | `9,980` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.79`** | `7.66` | `7.81` | **`0.997x`** | `1.005x` | `7.43` | **`1.05x`** | `10,044` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.27`** | `4.26` | `4.36` | **`0.979x`** | `1.000x` | `16.62` | **`0.26x`** | `10,044` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`11.01`** | `10.99` | `10.63` | **`1.036x`** | `1.025x` | `28.29` | **`0.39x`** | `10,240` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.77`** | `38.47` | `38.26` | **`1.013x`** | `1.002x` | `50.16` | **`0.77x`** | `12,668` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.80`** | `9.73` | `9.84` | **`0.996x`** | `1.022x` | `8.00` | **`1.23x`** | `12,668` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.89`** | `11.81` | `11.88` | **`1.001x`** | `0.991x` | `18.96` | **`0.63x`** | `12,668` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.20`** | `11.13` | `11.12` | **`1.007x`** | `0.988x` | `10.89` | **`1.03x`** | `12,852` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.94`** | `45.27` | `45.63` | **`1.007x`** | `1.015x` | `57.23` | **`0.80x`** | `18,212` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.14`** | `23.64` | `23.87` | **`1.011x`** | `1.017x` | `24.35` | **`0.99x`** | `18,292` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.87`** | `4.70` | `4.72` | **`1.030x`** | `0.992x` | `4.04` | **`1.21x`** | `18,324` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.69`** | `36.25` | `36.44` | **`1.007x`** | `1.004x` | `26.60` | **`1.38x`** | `18,292` | `fc7490e2b80c68e2` ✓ |

---

## 03_h2_eklemede_dengeleme — `performans-A-hizalama@0c65dbe`

**H2 — eklemede sayfa bölünmesi, hücre yerleşimi ve sayfa tahsisi C gibi.** Her B-tree eklemesi ve yerinde satır değiştirme artık C'nin `sqlite3BtreeInsert` yolunu izliyor (yeni `sqliters-btree/src/insert.rs`): aynı boyutta üzerine yazma (`btreeOverwriteCell`), `fillInCell` (yerel/taşma bölümü, taşma zincirinin tahsis sırası), var olan hücrenin değiştirilmesi, `insertCell` → `allocateSpace` (serbest bloktan ilk uyan, boşluk ya da birleştirme) ve sayfa taşınca H1'de port edilen `balance()` (`balance_quick`, üç kardeş arası dağıtım, `balance_deeper`). Hızlı yollar ham kaldı: boşluğa sığan ekleme C'de de yalnızca `insertCell`dir. Motorun kendi bölünme ve yeniden paketleme kodu (yaklaşık 3.500 satır) kaldırıldı. C'nin bıraktığı baytlar artık temizlenmiyor (yeni kök yalnız başlık; serbest listeden alınan sayfa yalnız C'nin sıfırladığı durumda sıfırlanıyor). Motor düzeyinde üç kural C'ye getirildi: WITHOUT ROWID `UPDATE` yalnız değişen ikincil indeksleri yazıyor; indekssiz tabloda `REPLACE` ve eski satırı okumayan iki geçişli `UPDATE` C'nin hücre değiştirme yolunu kullanıyor; ikincil indeksi/tetikleyicisi/yabancı anahtarı olmayan WITHOUT ROWID tabloda `REPLACE` çakışan satırı silmiyor (bir entegrasyon testi beklentisi buna göre değişti).

- **Testler**: `cargo test --workspace`: 792 geçti (11 yeni; 10'u C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile **bayt bayt aynı 58 → 195**; kötüleşen yok, 139 iyileşti. Kalan 50'yi engelleyenler: indeks toplu kurma/`VACUUM` (17, H3), `ALTER TABLE` şema satırı dizisi (4), R-Tree (18), FTS5 (7), dört tekil. **Karar eşliği** (iki motor boş dosyadan; 180 betik, 3.092.603 C olayı): 176 betikte olay dizisi, SQL çıktısı, bütünlük ve şekil aynı, 175'i bayt bayt aynı; uyuşmayan 4 betik H3'ün konusu. `balance_quick` 14.520 kez tetiklendi (H1'de 0). Tetiklenmeyen: `bBulk=1` dalları SQL üzerinden (bir alt düzeyde C'nin `CREATE INDEX` şekliyle doğrulandı), `BTREE_PREFORMAT`. Ayrıntı: `karar_esligi_h2.md`. **Sağlık senaryoları:** s01–s04 ve s06–s09 C ile bayt bayt aynı; sayfa sayıları s03, s04, s06–s11'de C ile eşit. **`secure_delete`:** `ON` iken dosyada kalan artık sayısı C ile aynı (1.955; 1.932 canlı) — C bölünmeden sonra sayfa boşluğunda eski bir kopya bırakıyor, motor artık aynısını yapıyor. **Farksal koşular:** 4.000 tohum, çıktı C ile aynı, her dosya C `integrity_check` geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** üç kipte 99/100, 94/94, 99/100 ("her motor kendi dosyasını kurar" kipi önce 49/100 idi). **Çökme/geri alma:** 0 hata. **Bedel:** toplam 1,006x; komut sayıları `W08` 0,980, `W10` 0,946, `W13` 0,986, `W14` 0,992 (azaldı), `W01` 1,001, `W06` 1,006.
- **Toplam**: `249.65 ms` (taban `247.49 ms`, C `294.16 ms`) — geomean `Rust/C` **`0.825x`** (disk WAL `0.795x`, bellek-içi `0.941x`)
- **Tepe RSS**: `18,160 KiB` (C: `6,164 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.61`** | `7.52` | `7.43` | **`1.024x`** | `1.012x` | `9.65` | **`0.79x`** | `9,148` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.51`** | `7.43` | `7.55` | **`0.995x`** | `1.005x` | `8.68` | **`0.86x`** | `10,116` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.86`** | `27.50` | `27.98` | **`0.996x`** | `1.006x` | `23.26` | **`1.20x`** | `10,116` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.88`** | `7.70` | `7.79` | **`1.011x`** | `1.016x` | `7.43` | **`1.06x`** | `10,180` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `4.27` | **`1.002x`** | `1.001x` | `16.62` | **`0.26x`** | `10,180` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.73`** | `10.70` | `11.01` | **`0.974x`** | `0.998x` | `28.29` | **`0.38x`** | `10,308` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.97`** | `38.46` | `38.77` | **`1.005x`** | `1.008x` | `50.16` | **`0.78x`** | `12,680` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.46`** | `9.34` | `9.80` | **`0.965x`** | `0.986x` | `8.00` | **`1.18x`** | `12,808` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.99`** | `11.86` | `11.89` | **`1.008x`** | `1.000x` | `18.96` | **`0.63x`** | `12,808` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.18`** | `11.11` | `11.20` | **`0.998x`** | `0.987x` | `10.89` | **`1.03x`** | `12,992` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.72`** | `45.72` | `45.94` | **`1.017x`** | `1.032x` | `57.23` | **`0.82x`** | `18,080` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.18`** | `23.52` | `24.14` | **`1.002x`** | `1.019x` | `24.35` | **`0.99x`** | `18,108` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.83`** | `4.58` | `4.87` | **`0.992x`** | `0.984x` | `4.04` | **`1.20x`** | `18,140` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.46`** | `36.05` | `36.69` | **`0.994x`** | `0.997x` | `26.60` | **`1.37x`** | `18,108` | `fc7490e2b80c68e2` ✓ |

---

## 04_h3_toplu_kurma — `performans-A-hizalama@fd7364d`

**H3 — `CREATE INDEX` / `REINDEX`, toplu kopyalama, `VACUUM` ve `ALTER TABLE` C'nin yürüttüğü gibi.** (1) **İndeks kurma**: şema satırı önce yazılıyor; anahtarlar kararlı bir sıralayıcıdan geçip C'nin toplu imleciyle (`BTREE_BULKLOAD` → `balance_nonroot(bBulk=1)`) sıralı ekleniyor; `UNIQUE` denetimi bir önceki anahtarla karşılaştırarak yapılıyor; `REINDEX` 3.54.1'in tek döngüsü. Motorun sayfaları doğrudan yazan kendi toplu yükleyicisi kaldırıldı (yapraklar %90 yerine C gibi ~%98–99 dolu). (2) **Aktarım optimizasyonu** (`INSERT INTO d SELECT * FROM s`): C'nin 31 koşulu aynı sırayla; satırlar yeniden kodlanmadan hücre hücre kopyalanıyor, indeksler toplu imleçle; satır kimliği kuralları C ile aynı. (3) **`VACUUM` / `VACUUM INTO`**: C'nin ifade sırası (tablolar, indeksler, tablo başına aktarım, kalan şema satırları, başlık değerleri, dosya kopyası). (4) **`ALTER TABLE`**: şema satırları C'nin iç içe `UPDATE` geçişleriyle yazılıyor; yeniden adlandırma `sqlite_stat1`e dokunmuyor. Ayrıca bir B-tree kuralı: az önce değiştirilen satırın bir sonraki anahtarına C imleci ilerleterek ulaşıyor ve aynı boyutlu kaydın üzerine yazıyor. İki entegrasyon testi beklentisi C'ye göre değişti (indeksli tabloda `VACUUM` örtük satır kimliklerini korur; yeniden adlandırmadan sonra `sqlite_stat1` eski adı tutar).

- **Testler**: `cargo test --workspace`: 810 geçti (18 yeni; 13'ü C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **195 → 215**; kötüleşen yok, 21 iyileşti (17 indeks/`VACUUM` ve 4 yeniden adlandırma senaryosunun hepsi L0). Kalan 30: R-Tree 20, FTS5 7, üç tekil (WITHOUT ROWID silme sırası, taşma sayfalarında önbellek bağımlı baytlar, özel kilit kipinde sayaç, `PRAGMA optimize`, rastgele satır kimliği). **Karar eşliği** (boş dosyadan; 322 betik, 6.514.115 C olayı): 321 betikte olay dizisi aynı, 320'si her yönüyle eşit ve bayt bayt aynı; `bBulk=1` dengeleme adımı C'de 13.016, motorda 13.016; aktarım koşullarının #1–#27 ve #29–#31'i eşit sayıda tetiklendi. Ayrıntı: `karar_esligi_h3.md`. **Sağlık:** `s05` (sonradan indeks) artık bayt bayt aynı, 458/458 sayfa (önce 490/458). **Farksal koşular:** yeni karışık koşu 5.000 tohum — stdout C ile aynı 4.997, her dosya iki yönde bütünlük kontrolünden geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** 99/100, 94/94, 99/100 (değişmedi). **Çökme/geri alma:** dört takım, 0 hata (yeni takım: indeks kurma, `VACUUM`, kopyalama, yeniden adlandırma; 372 betik + 3.794 kurtarma). **Bedel / kazanç:** toplam 0,993x; `W02` 7,45 → 5,64 ms (0,757x; C 8,76 ms). **Geri kazanılmayan:** 84 MB veritabanında beş toplu ifade C'nin 1,3–2,1 katı sürüyor (`VACUUM` 565 ms'ye karşı 393; kopyalama 438'e karşı 210) ve `CREATE INDEX`/`REINDEX` orada H2'den %8/%13 yavaş; H2'ye göre `VACUUM` 2,2 kat, kopyalama 6,1 kat hızlandı. **Hizalanmayan:** `sqlite_rename_quotefix` (yeniden adlandırmada çift tırnaklı dizgelerin çevrilmesi); tablo `UNIQUE` kısıtı ile `CREATE UNIQUE INDEX` ayrımı aktarım uyumluluğunda yok.
- **Toplam**: `246.82 ms` (taban `247.49 ms`, C `294.16 ms`) — geomean `Rust/C` **`0.808x`** (disk WAL `0.779x`, bellek-içi `0.925x`)
- **Tepe RSS**: `18,716 KiB` (C: `6,164 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.89`** | `7.63` | `7.61` | **`1.037x`** | `1.049x` | `9.65` | **`0.82x`** | `9,180` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.71`** | `5.62` | `7.51` | **`0.760x`** | `0.764x` | `8.68` | **`0.66x`** | `10,140` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.38`** | `28.03` | `27.86` | **`1.019x`** | `1.025x` | `23.26` | **`1.22x`** | `10,140` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.80`** | `7.65` | `7.88` | **`0.991x`** | `1.007x` | `7.43` | **`1.05x`** | `10,204` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.06`** | `4.04` | `4.28` | **`0.948x`** | `0.949x` | `16.62` | **`0.24x`** | `10,204` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.93`** | `10.85` | `10.73` | **`1.019x`** | `1.017x` | `28.29` | **`0.39x`** | `10,396` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`37.83`** | `37.55` | `38.97` | **`0.971x`** | `0.978x` | `50.16` | **`0.75x`** | `13,968` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.74`** | `9.62` | `9.46` | **`1.030x`** | `1.015x` | `8.00` | **`1.22x`** | `14,032` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.98`** | `11.91` | `11.99` | **`0.999x`** | `0.999x` | `18.96` | **`0.63x`** | `14,032` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.06`** | `10.85` | `11.18` | **`0.990x`** | `0.977x` | `10.89` | **`1.02x`** | `14,000` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.83`** | `45.06` | `46.72` | **`0.981x`** | `1.013x` | `57.23` | **`0.80x`** | `18,680` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.72`** | `23.45` | `24.18` | **`0.981x`** | `1.000x` | `24.35` | **`0.97x`** | `18,716` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.94`** | `4.62` | `4.83` | **`1.023x`** | `1.007x` | `4.04` | **`1.22x`** | `18,748` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.94`** | `36.36` | `36.46` | **`1.013x`** | `1.011x` | `26.60` | **`1.39x`** | `18,716` | `fc7490e2b80c68e2` ✓ |

---

## 05_h4_rtree — `performans-A-hizalama@b734244`

**H4 — R-Tree, C'nin `rtree.c` dosyasının portu olarak.** `crates/sqliters-ext/src/rtree.rs` yeniden yazıldı: ağaç algoritması (`ChooseLeaf`, `AdjustTree`, `SplitNode`/`splitNodeStartree`/`SortByDimension`, `float`/`double` hücre aritmetiği, silmede `removeNode` + `reinsertNodeContent`), düğüm nesneleri ve **yazma zamanı** (düğüm son başvurusu bırakılınca yazılır; ifadeler arası önbellek ve geri alma kaydı yok), sorgular (`rtreeBestIndex`, arama noktası kuyruğu — satır sırasını belirler, `SQLITE_LOCKED_VTAB`), `rtreecheck`. Motorun kendi ağaç algoritması, sınırsız düğüm önbelleği ve geri alma kaydı kaldırıldı. **Ana motor tarafında tüm sanal tablolar için C'nin kuralları**: değerler `xUpdate`e yakınlık uygulanmadan ulaşır; çok satırlı `DELETE` satır kümesiyle (artan satır kimliği); modülün iç ifadeleri üst düzey ifade gibi sayılır (`changes()`/`total_changes()`); `IN` ve `OR` planları C'nin maliyet modeliyle; sanal tablo taraması için EQP metni; sanal tablo yeniden adlandırma C'nin sırasıyla (ad tırnaklı, görünüm/tetikleyiciler yeniden yazılır). İki pager kuralı: serbest listeden alınan sayfa açık kayıt noktaları için saklanmaz; otomatik commit'te açılan `SAVEPOINT` pager kayıt noktası sayılmaz. Bir entegrasyon testi beklentisi C'ye göre değişti (yeniden adlandırılan sanal tablonun şema metninde ad tırnak içinde).

- **Testler**: `cargo test --workspace`: 832 geçti (25 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **215 → 233**; kötüleşen yok; R-Tree adlı 22 senaryonun hepsi L0. Kalan 12: FTS5 7, beş tekil. **Karar eşliği** (boş dosyadan; 130 R-Tree betiği, 715.577 karar olayı): 130/130 her yönüyle eşit ve bayt bayt aynı; tetiklenmeyen: geometri puanlı arama kuyruğu (geri çağırma yok), bozuk düğüm dalları. Ayrıntı: `karar_esligi_h4.md`. **Farksal koşular:** CLI 3.000 tohum — dosyalar bayt bayt aynı 2.418/2.418 (H3 düzeyinde 0), iki yönde `integrity_check` + `rtreecheck` tamam; API 1.000 tohum — çıktı ve dosyalar 1.000/1.000; karışık yazıcılar (C→Rust, Rust→C) 253 geçmişte son dosya C→C ile aynı. **Sayaçlar** her ifadeden sonra C ile aynı (ör. oluşturma 1/1, kimlikli ekleme toplam +3, güncelleme +5, silme +3). **Bağımsız kontrol:** 7.191 satırlık çok R-Tree'li rastgele betikte dosya, stdout ve stderr C ile aynı. **Sağlık:** s12 73/73 sayfa, 152/152 düğüm; s13 81/81; ikisi de bayt bayt aynı. **Bellek (tepe RSS):** tek işlemde 1e6 ekleme 267 MB → 61,6 MB (C 6,9 MB); modülün kendi belleği sınırlı, kalan pager'ın işlem sayfalarını bellekte tutmasından. 1e5 otomatik commit ekleme: 162,7 G → 8,87 G komut (C 5,06 G). **Çökme/geri alma:** beş takım, 0 hata. **Bedel:** `W12` 23,72 → 33,54 ms (1,414x; komut 1,284x; C 24,63 ms); `W11` 0,936x (ortak sanal tablo altyapısı ucuzladı); toplam 1,029x. Bedelin nedeni C'nin yazma zamanı: her eklemede yol düğümleri okunuyor ve gölge tablolara birkaç B-tree işlemi yapılıyor; C aynı işi 154,6 M komutla yapıyor, motor 190,4 M. **Hizalanmayan:** birleşimde sanal tablo varken birleşim sırası (C'nin yol çözücüsü; `ORDER BY`sız satır sırası ve EQP farklı), geometri/`MATCH` geri çağırmaları, geopoly. FTS5 sayaçları ve EQP `idxNum`/`idxStr` değerleri hâlâ C'ninki değil (FTS5 modülü bu adımda değişmedi).
- **Toplam**: `252.87 ms` (taban `247.49 ms`, C `294.16 ms`) — geomean `Rust/C` **`0.825x`** (disk WAL `0.779x`, bellek-içi `1.020x`)
- **Tepe RSS**: `18,688 KiB` (C: `6,164 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`8.06`** | `7.65` | `7.89` | **`1.021x`** | `1.072x` | `9.65` | **`0.83x`** | `9,040` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.68`** | `5.62` | `5.71` | **`0.996x`** | `0.761x` | `8.68` | **`0.66x`** | `10,000` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.05`** | `27.63` | `28.38` | **`0.989x`** | `1.013x` | `23.26` | **`1.21x`** | `10,000` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.81`** | `7.72` | `7.80` | **`1.001x`** | `1.008x` | `7.43` | **`1.05x`** | `10,064` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.10`** | `4.09` | `4.06` | **`1.010x`** | `0.959x` | `16.62` | **`0.25x`** | `10,064` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.69`** | `10.64` | `10.93` | **`0.978x`** | `0.995x` | `28.29` | **`0.38x`** | `10,192` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`37.64`** | `37.44` | `37.83` | **`0.995x`** | `0.973x` | `50.16` | **`0.75x`** | `13,700` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.89`** | `9.63` | `9.74` | **`1.015x`** | `1.031x` | `8.00` | **`1.24x`** | `13,764` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.06`** | `12.04` | `11.98` | **`1.007x`** | `1.006x` | `18.96` | **`0.64x`** | `13,764` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.35`** | `11.22` | `11.06` | **`1.026x`** | `1.002x` | `10.89` | **`1.04x`** | `13,732` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`42.88`** | `42.30` | `45.83` | **`0.936x`** | `0.947x` | `57.23` | **`0.75x`** | `18,452` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`33.10`** | `31.02` | `23.72` | **`1.395x`** | `1.395x` | `24.35` | **`1.36x`** | `18,508` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.90`** | `4.48` | `4.94` | **`0.994x`** | `1.000x` | `4.04` | **`1.22x`** | `18,540` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.64`** | `36.04` | `36.94` | **`0.992x`** | `1.002x` | `26.60` | **`1.38x`** | `18,508` | `fc7490e2b80c68e2` ✓ |
