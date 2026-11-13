# `performans-A-hizalama` — C'ye Hizalama Adımları (Bedelli) Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-13T21:27:43+03:00`
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
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`294.23`** | `—` | **`1.000x`** | **`1.000x`** | `6,392` | `—` |
| **00_performans_A_ucu** | `performans-A@225f5c2` | `7,299.0` | `6,157.8` | **`247.57`** | `1.000x` | **`0.821x`** | `0.841x` | `18,240` | 14/14 ✓ |
| **01_h1_silmede_dengeleme** | `performans-A-hizalama@034af4f` | `7,346.9` | `6,193.5` | **`249.00`** | `1.004x` | **`0.825x`** | `0.846x` | `18,288` | 14/14 ✓ |
| **02_h1b_dml_stratejisi** | `performans-A-hizalama@ebc3fde` | `7,461.8` | `6,294.8` | **`248.86`** | `1.003x` | **`0.824x`** | `0.846x` | `18,408` | 14/14 ✓ |
| **03_h2_eklemede_dengeleme** | `performans-A-hizalama@0c65dbe` | `7,376.3` | `6,223.2` | **`250.13`** | `1.006x` | **`0.826x`** | `0.850x` | `18,232` | 14/14 ✓ |
| **04_h3_toplu_kurma** | `performans-A-hizalama@fd7364d` | `7,363.6` | `6,217.5` | **`247.18`** | `0.986x` | **`0.810x`** | `0.840x` | `18,852` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_A_ucu | 01_h1_silmede_dengeleme | 02_h1b_dml_stratejisi | 03_h2_eklemede_dengeleme | 04_h3_toplu_kurma |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.70` | `7.55` (**0.78x**) | `7.53` (**0.78x**) | `7.43` (**0.77x**) | `7.61` (**0.78x**) | `7.86` (**0.81x**) |
| `W02_create_indexes` | `8.66` | `7.46` (**0.86x**) | `7.50` (**0.87x**) | `7.50` (**0.87x**) | `7.53` (**0.87x**) | `5.68` (**0.66x**) |
| `W03_pk_point_select` | `23.30` | `27.67` (**1.19x**) | `28.20` (**1.21x**) | `27.88` (**1.20x**) | `27.96` (**1.20x**) | `27.96` (**1.20x**) |
| `W04_secondary_idx_range` | `7.42` | `7.82` (**1.05x**) | `7.80` (**1.05x**) | `7.76` (**1.05x**) | `7.75` (**1.04x**) | `7.84` (**1.06x**) |
| `W05_full_scan_filter_agg` | `16.53` | `4.28` (**0.26x**) | `4.36` (**0.26x**) | `4.28` (**0.26x**) | `4.28` (**0.26x**) | `4.06` (**0.25x**) |
| `W06_join_group_order` | `28.21` | `10.76` (**0.38x**) | `10.63` (**0.38x**) | `10.99` (**0.39x**) | `10.73` (**0.38x**) | `10.90` (**0.39x**) |
| `W07_window_functions` | `50.17` | `38.71` (**0.77x**) | `38.58` (**0.77x**) | `38.71` (**0.77x**) | `38.90` (**0.78x**) | `38.04` (**0.76x**) |
| `W08_indexed_update_delete` | `7.97` | `9.57` (**1.20x**) | `9.90` (**1.24x**) | `9.77` (**1.23x**) | `9.38` (**1.18x**) | `9.79` (**1.23x**) |
| `W09_like_text_scan` | `18.95` | `11.95` (**0.63x**) | `11.90` (**0.63x**) | `11.82` (**0.62x**) | `12.14` (**0.64x**) | `11.97` (**0.63x**) |
| `W10_jsonb_extract_agg` | `10.90` | `11.24` (**1.03x**) | `11.12` (**1.02x**) | `11.18` (**1.03x**) | `11.16` (**1.02x**) | `11.05` (**1.01x**) |
| `W11_fts5_index_and_bm25` | `57.33` | `45.69` (**0.80x**) | `46.00` (**0.80x**) | `45.84` (**0.80x**) | `46.85` (**0.82x**) | `45.69` (**0.80x**) |
| `W12_rtree_insert_and_bbox` | `24.36` | `23.40` (**0.96x**) | `24.14` (**0.99x**) | `24.16` (**0.99x**) | `24.29` (**1.00x**) | `24.62` (**1.01x**) |
| `W13_autocommit_wal_txns` | `4.03` | `4.88` (**1.21x**) | `4.79` (**1.19x**) | `4.84` (**1.20x**) | `4.92` (**1.22x**) | `4.98` (**1.23x**) |
| `W14_cli_sql_script` | `26.69` | `36.59` (**1.37x**) | `36.54` (**1.37x**) | `36.69` (**1.38x**) | `36.64` (**1.37x**) | `36.77` (**1.38x**) |
| **Toplam** | **`294.23`** | **`247.57`** (**0.84x**) | **`249.00`** (**0.85x**) | **`248.86`** (**0.85x**) | **`250.13`** (**0.85x**) | **`247.18`** (**0.84x**) |

---

## 00_performans_A_ucu — `performans-A@225f5c2`

**Baz.** `performans-A` dalının bu dal ayrılırken ucu (adım 5j). Ayrışma bulucu, 234 senaryo, son dosya katmanı: L0 56, L2 43, L3 3, L4 58, L5 59, L6 15.

- **Testler**: `cargo test --workspace`: 757 geçti, bilinen 4 başarısız, 337 yok sayıldı.
- **Toplam**: `247.57 ms` (taban `247.57 ms`, C `294.23 ms`) — geomean `Rust/C` **`0.821x`** (disk WAL `0.795x`, bellek-içi `0.924x`)
- **Tepe RSS**: `18,240 KiB` (C: `6,392 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.55`** | `7.47` | `1.000x` | `9.70` | **`0.78x`** | `9,072` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.46`** | `7.39` | `1.000x` | `8.66` | **`0.86x`** | `9,912` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.67`** | `27.43` | `1.000x` | `23.30` | **`1.19x`** | `9,912` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.82`** | `7.65` | `1.000x` | `7.42` | **`1.05x`** | `9,976` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `1.000x` | `16.53` | **`0.26x`** | `10,040` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.76`** | `10.70` | `1.000x` | `28.21` | **`0.38x`** | `10,104` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.71`** | `38.27` | `1.000x` | `50.17` | **`0.77x`** | `12,528` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.57`** | `9.52` | `1.000x` | `7.97` | **`1.20x`** | `12,592` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.95`** | `11.83` | `1.000x` | `18.95` | **`0.63x`** | `12,592` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.24`** | `11.19` | `1.000x` | `10.90` | **`1.03x`** | `12,776` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.69`** | `44.88` | `1.000x` | `57.33` | **`0.80x`** | `18,040` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.40`** | `22.96` | `1.000x` | `24.36` | **`0.96x`** | `18,124` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.88`** | `4.82` | `1.000x` | `4.03` | **`1.21x`** | `18,156` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.59`** | `36.30` | `1.000x` | `26.69` | **`1.37x`** | `18,124` | `fc7490e2b80c68e2` ✓ |

---

## 01_h1_silmede_dengeleme — `performans-A-hizalama@034af4f`

**H1 — silmede C gibi yeniden dengeleme.** C, bir silmeden sonra sayfanın boş alanı kullanılabilir alanın üçte ikisini aşarsa `balance()` çağırır ve `balance_nonroot()` hücreleri en çok üç kardeş arasında yeniden dağıtır (gerekirse bir sayfayı serbest bırakır, kökte bir seviye düşürür); iç sayfadaki bir hücre silinince yerine öncül yaprak hücresi konur. Motor yalnızca yaprak tamamen boşaldığında davranıyordu; iç indeks hücresi silmede bazı durumlarda tüm indeksi yeniden kuruyordu. Yeni `sqliters-btree/src/balance.rs`, C'nin `balance` / `balance_nonroot` / `balance_deeper` / `balance_quick`, `insertCell` / `dropCell` / `allocateSpace` / `defragmentPage`, `editPage` / `rebuildPage` işlevlerinin portudur; tüm silme yolları (genel, konumlu, ham hızlı yollar, indeks) buna bağlandı. Hızlı yollar ham kaldı: üçte iki testi O(1) bir sınırla başlıyor. Ekleme tarafı bu adımda değişmedi (H2 aynı kodu etkinleştirecek). Motor düzeyinde bir düzeltme de gerekti: satır kimliği çakışmasında `REPLACE` artık C gibi yalnızca indeks girdilerini silip satırın üzerine yazıyor.

- **Testler**: `cargo test --workspace`: 764 geçti (7 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Karar eşliği** (iki motor aynı başlangıç dosyasından; 95 senaryo, 762.748 C olayı): 93 senaryoda her karar, SQL çıktısı, bütünlük ve sayfa sayfa şekil C ile aynı; kalan ikisi bu adımdan önce de var olan motor düzeyi farklar (indeks taramalı silmede ağaçlar arası yazma sırası; tetikleyici içi `DELETE`in satır ziyaret sırası). Hiç tetiklenmeyen kararlar: `balance_quick` ve `bBulk` dalları (yalnızca eklemede), bozuk girdi dalları. Ayrıntı: `karar_esligi_h1.md`. **Ayrışma bulucu** (245 senaryo, son derleme): kötüleşen yok, 24 iyileşti (L5 59 → 39, L2 44 → 55). **Sayfa sayıları (motor/C):** karışık 4096: 296 → 251 / 249; 512: 2456 → 2214 / 2173; 1024 + auto_vacuum: 1182 → 986 / 972; WAL: 281 → 244 / 244. Kalan fark ekleme tarafı bölünmelerden (H2). **Farksal koşu:** 4.800 tohum, çıktı C ile ve dal noktasıyla aynı, her dosya C `integrity_check` geçiyor. **Çökme/geri alma:** 288 betik + commit sırasında öldürülen 2.660 kurtarma, 0 hata. **Kurtarma eşliği (kapı tam karşılanmadı):** otomatik commit'te aynı şekilli 93 dosyanın 93'ünde C `.recover` çıktısı aynı; açık işlem içinde 99'un 84'ünde. Neden bu adımdan önce de var: motor açık işlemdeki her genel-yol ifadesi için ifade kayıt noktası açıyor (C yalnızca çok satırlı ve iptal olabilecek ifadelerde), bu yüzden serbest kalan sayfanın yazımı düşürülemiyor. **Bedel:** `W08` süre +%5,0 (ajanın koşusunda +%3,3), komut +%0,75; diğer iş yüklerinde komut sayısı aynı. Kalan maliyet: C sayfa başına boş alanı önbellekte tutuyor (`nFree`), port gerektiğinde yeniden hesaplıyor.
- **Toplam**: `249.00 ms` (taban `247.57 ms`, C `294.23 ms`) — geomean `Rust/C` **`0.825x`** (disk WAL `0.797x`, bellek-içi `0.933x`)
- **Tepe RSS**: `18,288 KiB` (C: `6,392 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.53`** | `7.47` | `7.55` | **`0.998x`** | `0.998x` | `9.70` | **`0.78x`** | `9,168` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.50`** | `7.42` | `7.46` | **`1.005x`** | `1.005x` | `8.66` | **`0.87x`** | `10,068` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.20`** | `27.65` | `27.67` | **`1.019x`** | `1.019x` | `23.30` | **`1.21x`** | `10,068` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.80`** | `7.74` | `7.82` | **`0.997x`** | `0.997x` | `7.42` | **`1.05x`** | `10,132` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.36`** | `4.36` | `4.28` | **`1.019x`** | `1.019x` | `16.53` | **`0.26x`** | `10,132` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.62` | `10.76` | **`0.988x`** | `0.988x` | `28.21` | **`0.38x`** | `10,260` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.58`** | `38.19` | `38.71` | **`0.997x`** | `0.997x` | `50.17` | **`0.77x`** | `12,688` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.90`** | `9.76` | `9.57` | **`1.034x`** | `1.034x` | `7.97` | **`1.24x`** | `12,688` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.90`** | `11.83` | `11.95` | **`0.996x`** | `0.996x` | `18.95` | **`0.63x`** | `12,688` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.12`** | `11.08` | `11.24` | **`0.990x`** | `0.990x` | `10.90` | **`1.02x`** | `12,868` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.00`** | `45.00` | `45.69` | **`1.007x`** | `1.007x` | `57.33` | **`0.80x`** | `18,088` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.14`** | `23.46` | `23.40` | **`1.032x`** | `1.032x` | `24.36` | **`0.99x`** | `18,108` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.79`** | `4.46` | `4.88` | **`0.980x`** | `0.980x` | `4.03` | **`1.19x`** | `18,140` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.54`** | `35.98` | `36.59` | **`0.999x`** | `0.999x` | `26.69` | **`1.37x`** | `18,108` | `fc7490e2b80c68e2` ✓ |

---

## 02_h1b_dml_stratejisi — `performans-A-hizalama@ebc3fde`

**H1b — DML ifadelerinin C'nin kod üreticisi gibi yürütülmesi.** (A) **Geçiş stratejisi ve satır ziyaret sırası**: `DELETE` ve `UPDATE` için C'nin kararları port edildi (tek geçiş / satır kümesi / geçici indeks; `bComplex`, `chngKey`, `hasFK`, kısaltma optimizasyonu koşulları); iki geçişte satırlar C gibi satır kimliği/birincil anahtar sırasıyla işleniyor. Tetikleyicili tabloda indeks üzerinden `DELETE` artık C ile aynı sonucu veriyor (önce sonuç farklıydı). `AUTOINCREMENT` ifade başına sayaç kullanıyor; `last_insert_rowid` ve `total_changes` başarısız ifadede C gibi geri alınmıyor; tek satırlık `INSERT`te eksik üst satır hemen hata veriyor. (B) **Kısıt kontrol sırası ve yazma sırası**: NOT NULL → CHECK → satır kimliği → UNIQUE indeksler (C'nin indeks sırasıyla; önce ters sıradaydı); `STRICT` tür kontrolü C'nin konumunda; `REPLACE` çakışan indeks girdisini en son siliyor; üretilmiş sütunlar bağımlılık sırasıyla hesaplanıyor (ileri başvuruda yanlış indeks girdisi yazılıyor ve C `integrity_check` düşüyordu). (C) **İfade günlüğü** yalnızca C'nin açtığı durumda açılıyor (`isMultiWrite && mayAbort`); başarısız ifadenin neyi geri aldığı `sqlite3VdbeHalt` kurallarına bağlandı. Bir entegrasyon testi beklentisi C'ye göre değişti (başarısız commit'ten sonra `last_insert_rowid`).

- **Testler**: `cargo test --workspace`: 781 geçti (17 yeni), bilinen 4 başarısız, 337 yok sayıldı. **DML farksalı** (rastgele şema, tetikleyici, yabancı anahtar, çakışma yan tümceleri; CLI + API; her ifadeden sonra `changes`/`total_changes`/`last_insert_rowid`): ajanın 3.000 tohumunda C ile birebir aynı 2.839; bağımsız 400 yeni tohumda (yalnız CLI) **377** — H1 ve dal noktası 134. Dal noktasında görülen 2 C `integrity_check` hatası kalmadı. Kalan farklar (örnekle sınıflandı): SELECT planlayıcısından gelen satır sırası (görünümler, WITHOUT ROWID indeks seçimi), DDL hata metinleri, iki yabancı anahtar ayrıntısı, `INSERT…SELECT`in eş-yordam durumu. **Karar izi** (600 tohum, 6.487 ifade): günlük açma 0 uyuşmazlık, durdurma sonucu 0, strateji 28, günlük bayrakları 9; B-tree yazma dizisi 6.184 eşit / 303 farklı. **Kurtarma eşliği:** açık işlem içinde 99/100 aynı (H1: 84/99); kalan durum serbest sayfa yolu farkı. **H1 karar izi:** 95/95 tam eşit (H1: 93). **Ayrışma bulucu:** dağılım aynı, kötüleşen yok. **Çökme/geri alma:** yeni ifade-iptali takımı 84 betik + 432 kurtarma, diğer iki takım hızlı kipte, 0 hata. **Bedel:** toplam 1,007x; komut sayıları 0,999–1,001, yalnız `W12` +%0,9 ve `W11` +%0,3 (ifade başına günlük politikası çağrısı). `W06` +%3,8 süre aynı komut sayısıyla (kod yerleşimi). **Hizalanmayan:** WITHOUT ROWID `UPDATE` tüm indeks girdilerini siliyor/yeniden ekliyor (C yalnız değişenleri); UPSERT'te silme sırası; C'nin eş-yordamlı `INSERT…SELECT`i; DDL/ANALYZE/PRAGMA ifade günlüğü bayrakları; 9 bayrak uyuşmazlığının ~5'inde C'nin günlük açtığı yerde motor açmıyor (farksalda gözlenmedi, dışlanmadı).
- **Toplam**: `248.86 ms` (taban `247.57 ms`, C `294.23 ms`) — geomean `Rust/C` **`0.824x`** (disk WAL `0.796x`, bellek-içi `0.933x`)
- **Tepe RSS**: `18,408 KiB` (C: `6,392 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.43`** | `7.37` | `7.53` | **`0.987x`** | `0.985x` | `9.70` | **`0.77x`** | `9,232` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.50`** | `7.49` | `7.50` | **`1.000x`** | `1.006x` | `8.66` | **`0.87x`** | `10,068` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.88`** | `27.40` | `28.20` | **`0.988x`** | `1.008x` | `23.30` | **`1.20x`** | `10,068` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.76`** | `7.68` | `7.80` | **`0.995x`** | `0.992x` | `7.42` | **`1.05x`** | `10,132` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `4.36` | **`0.980x`** | `0.998x` | `16.53` | **`0.26x`** | `10,132` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.99`** | `10.97` | `10.63` | **`1.034x`** | `1.022x` | `28.21` | **`0.39x`** | `10,260` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.71`** | `38.44` | `38.58` | **`1.003x`** | `1.000x` | `50.17` | **`0.77x`** | `12,752` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.77`** | `9.71` | `9.90` | **`0.987x`** | `1.020x` | `7.97` | **`1.23x`** | `12,816` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.82`** | `11.80` | `11.90` | **`0.993x`** | `0.989x` | `18.95` | **`0.62x`** | `12,816` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.18`** | `11.15` | `11.12` | **`1.005x`** | `0.995x` | `10.90` | **`1.03x`** | `13,000` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.84`** | `44.92` | `46.00` | **`0.996x`** | `1.003x` | `57.33` | **`0.80x`** | `18,096` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.16`** | `23.60` | `24.14` | **`1.001x`** | `1.032x` | `24.36` | **`0.99x`** | `18,140` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.84`** | `4.62` | `4.79` | **`1.010x`** | `0.990x` | `4.03` | **`1.20x`** | `18,172` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.69`** | `36.12` | `36.54` | **`1.004x`** | `1.003x` | `26.69` | **`1.38x`** | `18,140` | `fc7490e2b80c68e2` ✓ |

---

## 03_h2_eklemede_dengeleme — `performans-A-hizalama@0c65dbe`

**H2 — eklemede sayfa bölünmesi, hücre yerleşimi ve sayfa tahsisi C gibi.** Her B-tree eklemesi ve yerinde satır değiştirme artık C'nin `sqlite3BtreeInsert` yolunu izliyor (yeni `sqliters-btree/src/insert.rs`): aynı boyutta üzerine yazma (`btreeOverwriteCell`), `fillInCell` (yerel/taşma bölümü, taşma zincirinin tahsis sırası), var olan hücrenin değiştirilmesi, `insertCell` → `allocateSpace` (serbest bloktan ilk uyan, boşluk ya da birleştirme) ve sayfa taşınca H1'de port edilen `balance()` (`balance_quick`, üç kardeş arası dağıtım, `balance_deeper`). Hızlı yollar ham kaldı: boşluğa sığan ekleme C'de de yalnızca `insertCell`dir. Motorun kendi bölünme ve yeniden paketleme kodu (yaklaşık 3.500 satır) kaldırıldı. C'nin bıraktığı baytlar artık temizlenmiyor (yeni kök yalnız başlık; serbest listeden alınan sayfa yalnız C'nin sıfırladığı durumda sıfırlanıyor). Motor düzeyinde üç kural C'ye getirildi: WITHOUT ROWID `UPDATE` yalnız değişen ikincil indeksleri yazıyor; indekssiz tabloda `REPLACE` ve eski satırı okumayan iki geçişli `UPDATE` C'nin hücre değiştirme yolunu kullanıyor; ikincil indeksi/tetikleyicisi/yabancı anahtarı olmayan WITHOUT ROWID tabloda `REPLACE` çakışan satırı silmiyor (bir entegrasyon testi beklentisi buna göre değişti).

- **Testler**: `cargo test --workspace`: 792 geçti (11 yeni; 10'u C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile **bayt bayt aynı 58 → 195**; kötüleşen yok, 139 iyileşti. Kalan 50'yi engelleyenler: indeks toplu kurma/`VACUUM` (17, H3), `ALTER TABLE` şema satırı dizisi (4), R-Tree (18), FTS5 (7), dört tekil. **Karar eşliği** (iki motor boş dosyadan; 180 betik, 3.092.603 C olayı): 176 betikte olay dizisi, SQL çıktısı, bütünlük ve şekil aynı, 175'i bayt bayt aynı; uyuşmayan 4 betik H3'ün konusu. `balance_quick` 14.520 kez tetiklendi (H1'de 0). Tetiklenmeyen: `bBulk=1` dalları SQL üzerinden (bir alt düzeyde C'nin `CREATE INDEX` şekliyle doğrulandı), `BTREE_PREFORMAT`. Ayrıntı: `karar_esligi_h2.md`. **Sağlık senaryoları:** s01–s04 ve s06–s09 C ile bayt bayt aynı; sayfa sayıları s03, s04, s06–s11'de C ile eşit. **`secure_delete`:** `ON` iken dosyada kalan artık sayısı C ile aynı (1.955; 1.932 canlı) — C bölünmeden sonra sayfa boşluğunda eski bir kopya bırakıyor, motor artık aynısını yapıyor. **Farksal koşular:** 4.000 tohum, çıktı C ile aynı, her dosya C `integrity_check` geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** üç kipte 99/100, 94/94, 99/100 ("her motor kendi dosyasını kurar" kipi önce 49/100 idi). **Çökme/geri alma:** 0 hata. **Bedel:** toplam 1,006x; komut sayıları `W08` 0,980, `W10` 0,946, `W13` 0,986, `W14` 0,992 (azaldı), `W01` 1,001, `W06` 1,006.
- **Toplam**: `250.13 ms` (taban `247.57 ms`, C `294.23 ms`) — geomean `Rust/C` **`0.826x`** (disk WAL `0.797x`, bellek-içi `0.941x`)
- **Tepe RSS**: `18,232 KiB` (C: `6,392 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.61`** | `7.54` | `7.43` | **`1.024x`** | `1.009x` | `9.70` | **`0.78x`** | `9,176` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.53`** | `7.44` | `7.50` | **`1.003x`** | `1.009x` | `8.66` | **`0.87x`** | `10,016` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.96`** | `27.61` | `27.88` | **`1.003x`** | `1.010x` | `23.30` | **`1.20x`** | `10,016` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.75`** | `7.67` | `7.76` | **`0.998x`** | `0.991x` | `7.42` | **`1.04x`** | `10,080` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `4.28` | **`1.000x`** | `0.999x` | `16.53` | **`0.26x`** | `10,080` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.73`** | `10.71` | `10.99` | **`0.976x`** | `0.997x` | `28.21` | **`0.38x`** | `10,144` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.90`** | `38.55` | `38.71` | **`1.005x`** | `1.005x` | `50.17` | **`0.78x`** | `12,580` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.38`** | `9.34` | `9.77` | **`0.960x`** | `0.980x` | `7.97` | **`1.18x`** | `12,580` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.14`** | `11.85` | `11.82` | **`1.027x`** | `1.015x` | `18.95` | **`0.64x`** | `12,580` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.16`** | `11.06` | `11.18` | **`0.998x`** | `0.993x` | `10.90` | **`1.02x`** | `12,828` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.85`** | `46.10` | `45.84` | **`1.022x`** | `1.026x` | `57.33` | **`0.82x`** | `18,068` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.29`** | `23.57` | `24.16` | **`1.006x`** | `1.038x` | `24.36` | **`1.00x`** | `18,148` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.92`** | `4.75` | `4.84` | **`1.017x`** | `1.007x` | `4.03` | **`1.22x`** | `18,180` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.64`** | `36.22` | `36.69` | **`0.999x`** | `1.002x` | `26.69` | **`1.37x`** | `18,148` | `fc7490e2b80c68e2` ✓ |

---

## 04_h3_toplu_kurma — `performans-A-hizalama@fd7364d`

**H3 — `CREATE INDEX` / `REINDEX`, toplu kopyalama, `VACUUM` ve `ALTER TABLE` C'nin yürüttüğü gibi.** (1) **İndeks kurma**: şema satırı önce yazılıyor; anahtarlar kararlı bir sıralayıcıdan geçip C'nin toplu imleciyle (`BTREE_BULKLOAD` → `balance_nonroot(bBulk=1)`) sıralı ekleniyor; `UNIQUE` denetimi bir önceki anahtarla karşılaştırarak yapılıyor; `REINDEX` 3.54.1'in tek döngüsü. Motorun sayfaları doğrudan yazan kendi toplu yükleyicisi kaldırıldı (yapraklar %90 yerine C gibi ~%98–99 dolu). (2) **Aktarım optimizasyonu** (`INSERT INTO d SELECT * FROM s`): C'nin 31 koşulu aynı sırayla; satırlar yeniden kodlanmadan hücre hücre kopyalanıyor, indeksler toplu imleçle; satır kimliği kuralları C ile aynı. (3) **`VACUUM` / `VACUUM INTO`**: C'nin ifade sırası (tablolar, indeksler, tablo başına aktarım, kalan şema satırları, başlık değerleri, dosya kopyası). (4) **`ALTER TABLE`**: şema satırları C'nin iç içe `UPDATE` geçişleriyle yazılıyor; yeniden adlandırma `sqlite_stat1`e dokunmuyor. Ayrıca bir B-tree kuralı: az önce değiştirilen satırın bir sonraki anahtarına C imleci ilerleterek ulaşıyor ve aynı boyutlu kaydın üzerine yazıyor. İki entegrasyon testi beklentisi C'ye göre değişti (indeksli tabloda `VACUUM` örtük satır kimliklerini korur; yeniden adlandırmadan sonra `sqlite_stat1` eski adı tutar).

- **Testler**: `cargo test --workspace`: 810 geçti (18 yeni; 13'ü C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile bayt bayt aynı **195 → 215**; kötüleşen yok, 21 iyileşti (17 indeks/`VACUUM` ve 4 yeniden adlandırma senaryosunun hepsi L0). Kalan 30: R-Tree 20, FTS5 7, üç tekil (WITHOUT ROWID silme sırası, taşma sayfalarında önbellek bağımlı baytlar, özel kilit kipinde sayaç, `PRAGMA optimize`, rastgele satır kimliği). **Karar eşliği** (boş dosyadan; 322 betik, 6.514.115 C olayı): 321 betikte olay dizisi aynı, 320'si her yönüyle eşit ve bayt bayt aynı; `bBulk=1` dengeleme adımı C'de 13.016, motorda 13.016; aktarım koşullarının #1–#27 ve #29–#31'i eşit sayıda tetiklendi. Ayrıntı: `karar_esligi_h3.md`. **Sağlık:** `s05` (sonradan indeks) artık bayt bayt aynı, 458/458 sayfa (önce 490/458). **Farksal koşular:** yeni karışık koşu 5.000 tohum — stdout C ile aynı 4.997, her dosya iki yönde bütünlük kontrolünden geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** 99/100, 94/94, 99/100 (değişmedi). **Çökme/geri alma:** dört takım, 0 hata (yeni takım: indeks kurma, `VACUUM`, kopyalama, yeniden adlandırma; 372 betik + 3.794 kurtarma). **Bedel / kazanç:** toplam 0,993x; `W02` 7,45 → 5,64 ms (0,757x; C 8,76 ms). **Geri kazanılmayan:** 84 MB veritabanında beş toplu ifade C'nin 1,3–2,1 katı sürüyor (`VACUUM` 565 ms'ye karşı 393; kopyalama 438'e karşı 210) ve `CREATE INDEX`/`REINDEX` orada H2'den %8/%13 yavaş; H2'ye göre `VACUUM` 2,2 kat, kopyalama 6,1 kat hızlandı. **Hizalanmayan:** `sqlite_rename_quotefix` (yeniden adlandırmada çift tırnaklı dizgelerin çevrilmesi); tablo `UNIQUE` kısıtı ile `CREATE UNIQUE INDEX` ayrımı aktarım uyumluluğunda yok.
- **Toplam**: `247.18 ms` (taban `247.57 ms`, C `294.23 ms`) — geomean `Rust/C` **`0.810x`** (disk WAL `0.779x`, bellek-içi `0.935x`)
- **Tepe RSS**: `18,852 KiB` (C: `6,392 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.86`** | `7.63` | `7.61` | **`1.032x`** | `1.041x` | `9.70` | **`0.81x`** | `9,316` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`5.68`** | `5.65` | `7.53` | **`0.754x`** | `0.761x` | `8.66` | **`0.66x`** | `10,272` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.96`** | `27.52` | `27.96` | **`1.000x`** | `1.010x` | `23.30` | **`1.20x`** | `10,272` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.84`** | `7.68` | `7.75` | **`1.011x`** | `1.002x` | `7.42` | **`1.06x`** | `10,336` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.06`** | `4.05` | `4.28` | **`0.949x`** | `0.948x` | `16.53` | **`0.25x`** | `10,336` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.90`** | `10.85` | `10.73` | **`1.016x`** | `1.013x` | `28.21` | **`0.39x`** | `10,404` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.04`** | `37.59` | `38.90` | **`0.978x`** | `0.983x` | `50.17` | **`0.76x`** | `13,912` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.79`** | `9.71` | `9.38` | **`1.043x`** | `1.022x` | `7.97` | **`1.23x`** | `13,976` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.97`** | `11.94` | `12.14` | **`0.986x`** | `1.001x` | `18.95` | **`0.63x`** | `13,976` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.05`** | `10.96` | `11.16` | **`0.990x`** | `0.983x` | `10.90` | **`1.01x`** | `13,944` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.69`** | `45.27` | `46.85` | **`0.975x`** | `1.000x` | `57.33` | **`0.80x`** | `18,560` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.62`** | `23.29` | `24.29` | **`1.014x`** | `1.052x` | `24.36` | **`1.01x`** | `18,584` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.98`** | `4.76` | `4.92` | **`1.012x`** | `1.019x` | `4.03` | **`1.23x`** | `18,616` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.77`** | `36.29` | `36.64` | **`1.004x`** | `1.005x` | `26.69` | **`1.38x`** | `16,524` | `fc7490e2b80c68e2` ✓ |
