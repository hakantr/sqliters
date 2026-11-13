# `performans-A-hizalama` — C'ye Hizalama Adımları (Bedelli) Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-13T10:11:44+03:00`
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
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`295.08`** | `—` | **`1.000x`** | **`1.000x`** | `6,396` | `—` |
| **00_performans_A_ucu** | `performans-A@225f5c2` | `7,299.0` | `6,157.8` | **`249.57`** | `1.000x` | **`0.824x`** | `0.846x` | `18,268` | 14/14 ✓ |
| **01_h1_silmede_dengeleme** | `performans-A-hizalama@034af4f` | `7,346.9` | `6,193.5` | **`250.30`** | `1.003x` | **`0.827x`** | `0.848x` | `18,384` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_A_ucu | 01_h1_silmede_dengeleme |
| :--- | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.70` | `7.59` (**0.78x**) | `7.56` (**0.78x**) |
| `W02_create_indexes` | `8.72` | `7.44` (**0.85x**) | `7.58` (**0.87x**) |
| `W03_pk_point_select` | `23.35` | `28.07` (**1.20x**) | `27.99` (**1.20x**) |
| `W04_secondary_idx_range` | `7.44` | `7.78` (**1.05x**) | `7.91` (**1.06x**) |
| `W05_full_scan_filter_agg` | `16.59` | `4.29` (**0.26x**) | `4.38` (**0.26x**) |
| `W06_join_group_order` | `28.31` | `10.75` (**0.38x**) | `10.66` (**0.38x**) |
| `W07_window_functions` | `50.26` | `38.96` (**0.78x**) | `39.13` (**0.78x**) |
| `W08_indexed_update_delete` | `8.08` | `9.64` (**1.19x**) | `9.96` (**1.23x**) |
| `W09_like_text_scan` | `19.11` | `12.16` (**0.64x**) | `11.92` (**0.62x**) |
| `W10_jsonb_extract_agg` | `10.97` | `11.34` (**1.03x**) | `11.16` (**1.02x**) |
| `W11_fts5_index_and_bm25` | `57.61` | `46.14` (**0.80x**) | `46.38` (**0.81x**) |
| `W12_rtree_insert_and_bbox` | `24.43` | `23.42` (**0.96x**) | `24.26` (**0.99x**) |
| `W13_autocommit_wal_txns` | `4.07` | `5.01` (**1.23x**) | `4.91` (**1.21x**) |
| `W14_cli_sql_script` | `26.43` | `36.99` (**1.40x**) | `36.50` (**1.38x**) |
| **Toplam** | **`295.08`** | **`249.57`** (**0.85x**) | **`250.30`** (**0.85x**) |

---

## 00_performans_A_ucu — `performans-A@225f5c2`

**Baz.** `performans-A` dalının bu dal ayrılırken ucu (adım 5j). Ayrışma bulucu, 234 senaryo, son dosya katmanı: L0 56, L2 43, L3 3, L4 58, L5 59, L6 15.

- **Testler**: `cargo test --workspace`: 757 geçti, bilinen 4 başarısız, 337 yok sayıldı.
- **Toplam**: `249.57 ms` (taban `249.57 ms`, C `295.08 ms`) — geomean `Rust/C` **`0.824x`** (disk WAL `0.798x`, bellek-içi `0.926x`)
- **Tepe RSS**: `18,268 KiB` (C: `6,396 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.59`** | `7.49` | `1.000x` | `9.70` | **`0.78x`** | `9,168` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.44`** | `7.41` | `1.000x` | `8.72` | **`0.85x`** | `10,008` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.07`** | `27.66` | `1.000x` | `23.35` | **`1.20x`** | `10,008` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.78`** | `7.74` | `1.000x` | `7.44` | **`1.05x`** | `10,072` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.29`** | `4.27` | `1.000x` | `16.59` | **`0.26x`** | `10,072` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.75`** | `10.72` | `1.000x` | `28.31` | **`0.38x`** | `10,200` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.96`** | `38.69` | `1.000x` | `50.26` | **`0.78x`** | `12,688` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.64`** | `9.49` | `1.000x` | `8.08` | **`1.19x`** | `12,752` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.16`** | `11.83` | `1.000x` | `19.11` | **`0.64x`** | `12,752` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.34`** | `11.22` | `1.000x` | `10.97` | **`1.03x`** | `12,936` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.14`** | `45.27` | `1.000x` | `57.61` | **`0.80x`** | `18,100` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.42`** | `23.21` | `1.000x` | `24.43` | **`0.96x`** | `18,188` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`5.01`** | `4.55` | `1.000x` | `4.07` | **`1.23x`** | `18,220` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.99`** | `36.69` | `1.000x` | `26.43` | **`1.40x`** | `15,460` | `fc7490e2b80c68e2` ✓ |

---

## 01_h1_silmede_dengeleme — `performans-A-hizalama@034af4f`

**H1 — silmede C gibi yeniden dengeleme.** C, bir silmeden sonra sayfanın boş alanı kullanılabilir alanın üçte ikisini aşarsa `balance()` çağırır ve `balance_nonroot()` hücreleri en çok üç kardeş arasında yeniden dağıtır (gerekirse bir sayfayı serbest bırakır, kökte bir seviye düşürür); iç sayfadaki bir hücre silinince yerine öncül yaprak hücresi konur. Motor yalnızca yaprak tamamen boşaldığında davranıyordu; iç indeks hücresi silmede bazı durumlarda tüm indeksi yeniden kuruyordu. Yeni `sqliters-btree/src/balance.rs`, C'nin `balance` / `balance_nonroot` / `balance_deeper` / `balance_quick`, `insertCell` / `dropCell` / `allocateSpace` / `defragmentPage`, `editPage` / `rebuildPage` işlevlerinin portudur; tüm silme yolları (genel, konumlu, ham hızlı yollar, indeks) buna bağlandı. Hızlı yollar ham kaldı: üçte iki testi O(1) bir sınırla başlıyor. Ekleme tarafı bu adımda değişmedi (H2 aynı kodu etkinleştirecek). Motor düzeyinde bir düzeltme de gerekti: satır kimliği çakışmasında `REPLACE` artık C gibi yalnızca indeks girdilerini silip satırın üzerine yazıyor.

- **Testler**: `cargo test --workspace`: 764 geçti (7 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Karar eşliği** (iki motor aynı başlangıç dosyasından; 95 senaryo, 762.748 C olayı): 93 senaryoda her karar, SQL çıktısı, bütünlük ve sayfa sayfa şekil C ile aynı; kalan ikisi bu adımdan önce de var olan motor düzeyi farklar (indeks taramalı silmede ağaçlar arası yazma sırası; tetikleyici içi `DELETE`in satır ziyaret sırası). Hiç tetiklenmeyen kararlar: `balance_quick` ve `bBulk` dalları (yalnızca eklemede), bozuk girdi dalları. Ayrıntı: `karar_esligi_h1.md`. **Ayrışma bulucu** (245 senaryo, son derleme): kötüleşen yok, 24 iyileşti (L5 59 → 39, L2 44 → 55). **Sayfa sayıları (motor/C):** karışık 4096: 296 → 251 / 249; 512: 2456 → 2214 / 2173; 1024 + auto_vacuum: 1182 → 986 / 972; WAL: 281 → 244 / 244. Kalan fark ekleme tarafı bölünmelerden (H2). **Farksal koşu:** 4.800 tohum, çıktı C ile ve dal noktasıyla aynı, her dosya C `integrity_check` geçiyor. **Çökme/geri alma:** 288 betik + commit sırasında öldürülen 2.660 kurtarma, 0 hata. **Kurtarma eşliği (kapı tam karşılanmadı):** otomatik commit'te aynı şekilli 93 dosyanın 93'ünde C `.recover` çıktısı aynı; açık işlem içinde 99'un 84'ünde. Neden bu adımdan önce de var: motor açık işlemdeki her genel-yol ifadesi için ifade kayıt noktası açıyor (C yalnızca çok satırlı ve iptal olabilecek ifadelerde), bu yüzden serbest kalan sayfanın yazımı düşürülemiyor. **Bedel:** `W08` süre +%5,0 (ajanın koşusunda +%3,3), komut +%0,75; diğer iş yüklerinde komut sayısı aynı. Kalan maliyet: C sayfa başına boş alanı önbellekte tutuyor (`nFree`), port gerektiğinde yeniden hesaplıyor.
- **Toplam**: `250.30 ms` (taban `249.57 ms`, C `295.08 ms`) — geomean `Rust/C` **`0.827x`** (disk WAL `0.800x`, bellek-içi `0.933x`)
- **Tepe RSS**: `18,384 KiB` (C: `6,396 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.56`** | `7.47` | `7.59` | **`0.996x`** | `0.996x` | `9.70` | **`0.78x`** | `9,176` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.58`** | `7.50` | `7.44` | **`1.019x`** | `1.019x` | `8.72` | **`0.87x`** | `10,012` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.99`** | `27.66` | `28.07` | **`0.997x`** | `0.997x` | `23.35` | **`1.20x`** | `10,012` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.91`** | `7.70` | `7.78` | **`1.016x`** | `1.016x` | `7.44` | **`1.06x`** | `10,140` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.38`** | `4.36` | `4.29` | **`1.021x`** | `1.021x` | `16.59` | **`0.26x`** | `10,140` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.66`** | `10.63` | `10.75` | **`0.991x`** | `0.991x` | `28.31` | **`0.38x`** | `10,204` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`39.13`** | `38.46` | `38.96` | **`1.004x`** | `1.004x` | `50.26` | **`0.78x`** | `12,632` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.96`** | `9.80` | `9.64` | **`1.034x`** | `1.034x` | `8.08` | **`1.23x`** | `12,632` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.92`** | `11.87` | `12.16` | **`0.981x`** | `0.981x` | `19.11` | **`0.62x`** | `12,632` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.16`** | `11.10` | `11.34` | **`0.984x`** | `0.984x` | `10.97` | **`1.02x`** | `12,876` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.38`** | `44.99` | `46.14` | **`1.005x`** | `1.005x` | `57.61` | **`0.81x`** | `18,000` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.26`** | `23.28` | `23.42` | **`1.036x`** | `1.036x` | `24.43` | **`0.99x`** | `18,060` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.91`** | `4.79` | `5.01` | **`0.979x`** | `0.979x` | `4.07` | **`1.21x`** | `18,092` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.50`** | `35.97` | `36.99` | **`0.987x`** | `0.987x` | `26.43` | **`1.38x`** | `18,060` | `fc7490e2b80c68e2` ✓ |
