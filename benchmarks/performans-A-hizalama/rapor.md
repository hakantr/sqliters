# `performans-A-hizalama` — C'ye Hizalama Adımları (Bedelli) Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-13T16:22:51+03:00`
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
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`295.97`** | `—` | **`1.000x`** | **`1.000x`** | `6,240` | `—` |
| **00_performans_A_ucu** | `performans-A@225f5c2` | `7,299.0` | `6,157.8` | **`247.67`** | `1.000x` | **`0.819x`** | `0.837x` | `18,244` | 14/14 ✓ |
| **01_h1_silmede_dengeleme** | `performans-A-hizalama@034af4f` | `7,346.9` | `6,193.5` | **`248.14`** | `1.001x` | **`0.820x`** | `0.838x` | `18,372` | 14/14 ✓ |
| **02_h1b_dml_stratejisi** | `performans-A-hizalama@ebc3fde` | `7,461.8` | `6,294.8` | **`249.62`** | `1.003x` | **`0.821x`** | `0.843x` | `18,324` | 14/14 ✓ |
| **03_h2_eklemede_dengeleme** | `performans-A-hizalama@0c65dbe` | `7,376.3` | `6,223.2` | **`249.87`** | `1.002x` | **`0.820x`** | `0.844x` | `18,308` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_A_ucu | 01_h1_silmede_dengeleme | 02_h1b_dml_stratejisi | 03_h2_eklemede_dengeleme |
| :--- | ---: | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.71` | `7.59` (**0.78x**) | `7.57` (**0.78x**) | `7.42` (**0.76x**) | `7.61` (**0.78x**) |
| `W02_create_indexes` | `8.69` | `7.49` (**0.86x**) | `7.53` (**0.87x**) | `7.49` (**0.86x**) | `7.47` (**0.86x**) |
| `W03_pk_point_select` | `23.28` | `28.05` (**1.21x**) | `28.01` (**1.20x**) | `27.61` (**1.19x**) | `27.77` (**1.19x**) |
| `W04_secondary_idx_range` | `7.43` | `7.76` (**1.04x**) | `7.82` (**1.05x**) | `7.72` (**1.04x**) | `7.80` (**1.05x**) |
| `W05_full_scan_filter_agg` | `16.57` | `4.29` (**0.26x**) | `4.37` (**0.26x**) | `4.26` (**0.26x**) | `4.28` (**0.26x**) |
| `W06_join_group_order` | `28.17` | `10.75` (**0.38x**) | `10.63` (**0.38x**) | `11.02` (**0.39x**) | `10.72` (**0.38x**) |
| `W07_window_functions` | `50.31` | `38.72` (**0.77x**) | `38.63` (**0.77x**) | `38.91` (**0.77x**) | `38.89` (**0.77x**) |
| `W08_indexed_update_delete` | `8.05` | `9.54` (**1.19x**) | `9.96` (**1.24x**) | `10.09` (**1.25x**) | `9.55` (**1.19x**) |
| `W09_like_text_scan` | `19.10` | `11.92` (**0.62x**) | `12.10` (**0.63x**) | `11.85` (**0.62x**) | `11.91` (**0.62x**) |
| `W10_jsonb_extract_agg` | `11.00` | `11.22` (**1.02x**) | `11.16` (**1.01x**) | `11.19` (**1.02x**) | `11.18` (**1.02x**) |
| `W11_fts5_index_and_bm25` | `57.91` | `45.33` (**0.78x**) | `45.38` (**0.78x**) | `46.03` (**0.80x**) | `46.74` (**0.81x**) |
| `W12_rtree_insert_and_bbox` | `25.00` | `23.39` (**0.94x**) | `23.77` (**0.95x**) | `24.41` (**0.98x**) | `24.37` (**0.97x**) |
| `W13_autocommit_wal_txns` | `4.11` | `5.15` (**1.25x**) | `4.79` (**1.17x**) | `4.90` (**1.19x**) | `4.94` (**1.20x**) |
| `W14_cli_sql_script` | `26.64` | `36.48` (**1.37x**) | `36.42` (**1.37x**) | `36.71` (**1.38x**) | `36.64` (**1.38x**) |
| **Toplam** | **`295.97`** | **`247.67`** (**0.84x**) | **`248.14`** (**0.84x**) | **`249.62`** (**0.84x**) | **`249.87`** (**0.84x**) |

---

## 00_performans_A_ucu — `performans-A@225f5c2`

**Baz.** `performans-A` dalının bu dal ayrılırken ucu (adım 5j). Ayrışma bulucu, 234 senaryo, son dosya katmanı: L0 56, L2 43, L3 3, L4 58, L5 59, L6 15.

- **Testler**: `cargo test --workspace`: 757 geçti, bilinen 4 başarısız, 337 yok sayıldı.
- **Toplam**: `247.67 ms` (taban `247.67 ms`, C `295.97 ms`) — geomean `Rust/C` **`0.819x`** (disk WAL `0.797x`, bellek-içi `0.908x`)
- **Tepe RSS**: `18,244 KiB` (C: `6,240 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.59`** | `7.50` | `1.000x` | `9.71` | **`0.78x`** | `9,176` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.49`** | `7.42` | `1.000x` | `8.69` | **`0.86x`** | `10,016` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.05`** | `27.62` | `1.000x` | `23.28` | **`1.21x`** | `10,016` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.76`** | `7.70` | `1.000x` | `7.43` | **`1.04x`** | `10,080` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.29`** | `4.26` | `1.000x` | `16.57` | **`0.26x`** | `10,080` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.75`** | `10.73` | `1.000x` | `28.17` | **`0.38x`** | `10,208` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.72`** | `38.46` | `1.000x` | `50.31` | **`0.77x`** | `12,696` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.54`** | `9.50` | `1.000x` | `8.05` | **`1.19x`** | `12,760` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.92`** | `11.80` | `1.000x` | `19.10` | **`0.62x`** | `12,760` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.22`** | `11.16` | `1.000x` | `11.00` | **`1.02x`** | `12,944` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.33`** | `44.92` | `1.000x` | `57.91` | **`0.78x`** | `18,088` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.39`** | `23.07` | `1.000x` | `25.00` | **`0.94x`** | `18,164` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`5.15`** | `4.78` | `1.000x` | `4.11` | **`1.25x`** | `18,196` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.48`** | `36.27` | `1.000x` | `26.64` | **`1.37x`** | `18,164` | `fc7490e2b80c68e2` ✓ |

---

## 01_h1_silmede_dengeleme — `performans-A-hizalama@034af4f`

**H1 — silmede C gibi yeniden dengeleme.** C, bir silmeden sonra sayfanın boş alanı kullanılabilir alanın üçte ikisini aşarsa `balance()` çağırır ve `balance_nonroot()` hücreleri en çok üç kardeş arasında yeniden dağıtır (gerekirse bir sayfayı serbest bırakır, kökte bir seviye düşürür); iç sayfadaki bir hücre silinince yerine öncül yaprak hücresi konur. Motor yalnızca yaprak tamamen boşaldığında davranıyordu; iç indeks hücresi silmede bazı durumlarda tüm indeksi yeniden kuruyordu. Yeni `sqliters-btree/src/balance.rs`, C'nin `balance` / `balance_nonroot` / `balance_deeper` / `balance_quick`, `insertCell` / `dropCell` / `allocateSpace` / `defragmentPage`, `editPage` / `rebuildPage` işlevlerinin portudur; tüm silme yolları (genel, konumlu, ham hızlı yollar, indeks) buna bağlandı. Hızlı yollar ham kaldı: üçte iki testi O(1) bir sınırla başlıyor. Ekleme tarafı bu adımda değişmedi (H2 aynı kodu etkinleştirecek). Motor düzeyinde bir düzeltme de gerekti: satır kimliği çakışmasında `REPLACE` artık C gibi yalnızca indeks girdilerini silip satırın üzerine yazıyor.

- **Testler**: `cargo test --workspace`: 764 geçti (7 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Karar eşliği** (iki motor aynı başlangıç dosyasından; 95 senaryo, 762.748 C olayı): 93 senaryoda her karar, SQL çıktısı, bütünlük ve sayfa sayfa şekil C ile aynı; kalan ikisi bu adımdan önce de var olan motor düzeyi farklar (indeks taramalı silmede ağaçlar arası yazma sırası; tetikleyici içi `DELETE`in satır ziyaret sırası). Hiç tetiklenmeyen kararlar: `balance_quick` ve `bBulk` dalları (yalnızca eklemede), bozuk girdi dalları. Ayrıntı: `karar_esligi_h1.md`. **Ayrışma bulucu** (245 senaryo, son derleme): kötüleşen yok, 24 iyileşti (L5 59 → 39, L2 44 → 55). **Sayfa sayıları (motor/C):** karışık 4096: 296 → 251 / 249; 512: 2456 → 2214 / 2173; 1024 + auto_vacuum: 1182 → 986 / 972; WAL: 281 → 244 / 244. Kalan fark ekleme tarafı bölünmelerden (H2). **Farksal koşu:** 4.800 tohum, çıktı C ile ve dal noktasıyla aynı, her dosya C `integrity_check` geçiyor. **Çökme/geri alma:** 288 betik + commit sırasında öldürülen 2.660 kurtarma, 0 hata. **Kurtarma eşliği (kapı tam karşılanmadı):** otomatik commit'te aynı şekilli 93 dosyanın 93'ünde C `.recover` çıktısı aynı; açık işlem içinde 99'un 84'ünde. Neden bu adımdan önce de var: motor açık işlemdeki her genel-yol ifadesi için ifade kayıt noktası açıyor (C yalnızca çok satırlı ve iptal olabilecek ifadelerde), bu yüzden serbest kalan sayfanın yazımı düşürülemiyor. **Bedel:** `W08` süre +%5,0 (ajanın koşusunda +%3,3), komut +%0,75; diğer iş yüklerinde komut sayısı aynı. Kalan maliyet: C sayfa başına boş alanı önbellekte tutuyor (`nFree`), port gerektiğinde yeniden hesaplıyor.
- **Toplam**: `248.14 ms` (taban `247.67 ms`, C `295.97 ms`) — geomean `Rust/C` **`0.820x`** (disk WAL `0.796x`, bellek-içi `0.911x`)
- **Tepe RSS**: `18,372 KiB` (C: `6,240 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.57`** | `7.50` | `7.59` | **`0.997x`** | `0.997x` | `9.71` | **`0.78x`** | `9,148` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.53`** | `7.50` | `7.49` | **`1.005x`** | `1.005x` | `8.69` | **`0.87x`** | `9,984` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.01`** | `27.68` | `28.05` | **`0.998x`** | `0.998x` | `23.28` | **`1.20x`** | `9,984` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.82`** | `7.68` | `7.76` | **`1.008x`** | `1.008x` | `7.43` | **`1.05x`** | `10,048` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.37`** | `4.35` | `4.29` | **`1.020x`** | `1.020x` | `16.57` | **`0.26x`** | `10,048` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.60` | `10.75` | **`0.989x`** | `0.989x` | `28.17` | **`0.38x`** | `10,176` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.63`** | `38.13` | `38.72` | **`0.998x`** | `0.998x` | `50.31` | **`0.77x`** | `12,668` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.96`** | `9.79` | `9.54` | **`1.044x`** | `1.044x` | `8.05` | **`1.24x`** | `12,732` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.10`** | `11.86` | `11.92` | **`1.015x`** | `1.015x` | `19.10` | **`0.63x`** | `12,732` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.16`** | `11.08` | `11.22` | **`0.994x`** | `0.994x` | `11.00` | **`1.01x`** | `12,912` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.38`** | `45.07` | `45.33` | **`1.001x`** | `1.001x` | `57.91` | **`0.78x`** | `18,072` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.77`** | `23.35` | `23.39` | **`1.017x`** | `1.017x` | `25.00` | **`0.95x`** | `18,120` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.79`** | `4.67` | `5.15` | **`0.932x`** | `0.932x` | `4.11` | **`1.17x`** | `18,152` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.42`** | `36.17` | `36.48` | **`0.999x`** | `0.999x` | `26.64` | **`1.37x`** | `18,120` | `fc7490e2b80c68e2` ✓ |

---

## 02_h1b_dml_stratejisi — `performans-A-hizalama@ebc3fde`

**H1b — DML ifadelerinin C'nin kod üreticisi gibi yürütülmesi.** (A) **Geçiş stratejisi ve satır ziyaret sırası**: `DELETE` ve `UPDATE` için C'nin kararları port edildi (tek geçiş / satır kümesi / geçici indeks; `bComplex`, `chngKey`, `hasFK`, kısaltma optimizasyonu koşulları); iki geçişte satırlar C gibi satır kimliği/birincil anahtar sırasıyla işleniyor. Tetikleyicili tabloda indeks üzerinden `DELETE` artık C ile aynı sonucu veriyor (önce sonuç farklıydı). `AUTOINCREMENT` ifade başına sayaç kullanıyor; `last_insert_rowid` ve `total_changes` başarısız ifadede C gibi geri alınmıyor; tek satırlık `INSERT`te eksik üst satır hemen hata veriyor. (B) **Kısıt kontrol sırası ve yazma sırası**: NOT NULL → CHECK → satır kimliği → UNIQUE indeksler (C'nin indeks sırasıyla; önce ters sıradaydı); `STRICT` tür kontrolü C'nin konumunda; `REPLACE` çakışan indeks girdisini en son siliyor; üretilmiş sütunlar bağımlılık sırasıyla hesaplanıyor (ileri başvuruda yanlış indeks girdisi yazılıyor ve C `integrity_check` düşüyordu). (C) **İfade günlüğü** yalnızca C'nin açtığı durumda açılıyor (`isMultiWrite && mayAbort`); başarısız ifadenin neyi geri aldığı `sqlite3VdbeHalt` kurallarına bağlandı. Bir entegrasyon testi beklentisi C'ye göre değişti (başarısız commit'ten sonra `last_insert_rowid`).

- **Testler**: `cargo test --workspace`: 781 geçti (17 yeni), bilinen 4 başarısız, 337 yok sayıldı. **DML farksalı** (rastgele şema, tetikleyici, yabancı anahtar, çakışma yan tümceleri; CLI + API; her ifadeden sonra `changes`/`total_changes`/`last_insert_rowid`): ajanın 3.000 tohumunda C ile birebir aynı 2.839; bağımsız 400 yeni tohumda (yalnız CLI) **377** — H1 ve dal noktası 134. Dal noktasında görülen 2 C `integrity_check` hatası kalmadı. Kalan farklar (örnekle sınıflandı): SELECT planlayıcısından gelen satır sırası (görünümler, WITHOUT ROWID indeks seçimi), DDL hata metinleri, iki yabancı anahtar ayrıntısı, `INSERT…SELECT`in eş-yordam durumu. **Karar izi** (600 tohum, 6.487 ifade): günlük açma 0 uyuşmazlık, durdurma sonucu 0, strateji 28, günlük bayrakları 9; B-tree yazma dizisi 6.184 eşit / 303 farklı. **Kurtarma eşliği:** açık işlem içinde 99/100 aynı (H1: 84/99); kalan durum serbest sayfa yolu farkı. **H1 karar izi:** 95/95 tam eşit (H1: 93). **Ayrışma bulucu:** dağılım aynı, kötüleşen yok. **Çökme/geri alma:** yeni ifade-iptali takımı 84 betik + 432 kurtarma, diğer iki takım hızlı kipte, 0 hata. **Bedel:** toplam 1,007x; komut sayıları 0,999–1,001, yalnız `W12` +%0,9 ve `W11` +%0,3 (ifade başına günlük politikası çağrısı). `W06` +%3,8 süre aynı komut sayısıyla (kod yerleşimi). **Hizalanmayan:** WITHOUT ROWID `UPDATE` tüm indeks girdilerini siliyor/yeniden ekliyor (C yalnız değişenleri); UPSERT'te silme sırası; C'nin eş-yordamlı `INSERT…SELECT`i; DDL/ANALYZE/PRAGMA ifade günlüğü bayrakları; 9 bayrak uyuşmazlığının ~5'inde C'nin günlük açtığı yerde motor açmıyor (farksalda gözlenmedi, dışlanmadı).
- **Toplam**: `249.62 ms` (taban `247.67 ms`, C `295.97 ms`) — geomean `Rust/C` **`0.821x`** (disk WAL `0.795x`, bellek-içi `0.924x`)
- **Tepe RSS**: `18,324 KiB` (C: `6,240 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.42`** | `7.38` | `7.57` | **`0.979x`** | `0.977x` | `9.71` | **`0.76x`** | `9,208` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.49`** | `7.45` | `7.53` | **`0.995x`** | `1.000x` | `8.69` | **`0.86x`** | `10,108` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.61`** | `27.40` | `28.01` | **`0.986x`** | `0.984x` | `23.28` | **`1.19x`** | `10,108` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.72`** | `7.63` | `7.82` | **`0.987x`** | `0.995x` | `7.43` | **`1.04x`** | `10,172` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.26`** | `4.25` | `4.37` | **`0.975x`** | `0.995x` | `16.57` | **`0.26x`** | `10,172` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`11.02`** | `10.98` | `10.63` | **`1.037x`** | `1.025x` | `28.17` | **`0.39x`** | `10,364` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.91`** | `38.47` | `38.63` | **`1.007x`** | `1.005x` | `50.31` | **`0.77x`** | `12,792` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`10.09`** | `9.73` | `9.96` | **`1.014x`** | `1.058x` | `8.05` | **`1.25x`** | `12,792` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.85`** | `11.82` | `12.10` | **`0.980x`** | `0.994x` | `19.10` | **`0.62x`** | `12,792` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.19`** | `11.14` | `11.16` | **`1.003x`** | `0.997x` | `11.00` | **`1.02x`** | `12,976` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.03`** | `45.54` | `45.38` | **`1.014x`** | `1.016x` | `57.91` | **`0.80x`** | `18,236` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.41`** | `23.47` | `23.77` | **`1.027x`** | `1.044x` | `25.00` | **`0.98x`** | `18,276` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.90`** | `4.63` | `4.79` | **`1.021x`** | `0.952x` | `4.11` | **`1.19x`** | `16,180` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.71`** | `36.42` | `36.42` | **`1.008x`** | `1.007x` | `26.64` | **`1.38x`** | `16,148` | `fc7490e2b80c68e2` ✓ |

---

## 03_h2_eklemede_dengeleme — `performans-A-hizalama@0c65dbe`

**H2 — eklemede sayfa bölünmesi, hücre yerleşimi ve sayfa tahsisi C gibi.** Her B-tree eklemesi ve yerinde satır değiştirme artık C'nin `sqlite3BtreeInsert` yolunu izliyor (yeni `sqliters-btree/src/insert.rs`): aynı boyutta üzerine yazma (`btreeOverwriteCell`), `fillInCell` (yerel/taşma bölümü, taşma zincirinin tahsis sırası), var olan hücrenin değiştirilmesi, `insertCell` → `allocateSpace` (serbest bloktan ilk uyan, boşluk ya da birleştirme) ve sayfa taşınca H1'de port edilen `balance()` (`balance_quick`, üç kardeş arası dağıtım, `balance_deeper`). Hızlı yollar ham kaldı: boşluğa sığan ekleme C'de de yalnızca `insertCell`dir. Motorun kendi bölünme ve yeniden paketleme kodu (yaklaşık 3.500 satır) kaldırıldı. C'nin bıraktığı baytlar artık temizlenmiyor (yeni kök yalnız başlık; serbest listeden alınan sayfa yalnız C'nin sıfırladığı durumda sıfırlanıyor). Motor düzeyinde üç kural C'ye getirildi: WITHOUT ROWID `UPDATE` yalnız değişen ikincil indeksleri yazıyor; indekssiz tabloda `REPLACE` ve eski satırı okumayan iki geçişli `UPDATE` C'nin hücre değiştirme yolunu kullanıyor; ikincil indeksi/tetikleyicisi/yabancı anahtarı olmayan WITHOUT ROWID tabloda `REPLACE` çakışan satırı silmiyor (bir entegrasyon testi beklentisi buna göre değişti).

- **Testler**: `cargo test --workspace`: 792 geçti (11 yeni; 10'u C'nin yazdığı dosyanın boyut ve özetiyle eşitlik), bilinen 4 başarısız, 337 yok sayıldı. **Ayrışma bulucu** (245 senaryo): C ile **bayt bayt aynı 58 → 195**; kötüleşen yok, 139 iyileşti. Kalan 50'yi engelleyenler: indeks toplu kurma/`VACUUM` (17, H3), `ALTER TABLE` şema satırı dizisi (4), R-Tree (18), FTS5 (7), dört tekil. **Karar eşliği** (iki motor boş dosyadan; 180 betik, 3.092.603 C olayı): 176 betikte olay dizisi, SQL çıktısı, bütünlük ve şekil aynı, 175'i bayt bayt aynı; uyuşmayan 4 betik H3'ün konusu. `balance_quick` 14.520 kez tetiklendi (H1'de 0). Tetiklenmeyen: `bBulk=1` dalları SQL üzerinden (bir alt düzeyde C'nin `CREATE INDEX` şekliyle doğrulandı), `BTREE_PREFORMAT`. Ayrıntı: `karar_esligi_h2.md`. **Sağlık senaryoları:** s01–s04 ve s06–s09 C ile bayt bayt aynı; sayfa sayıları s03, s04, s06–s11'de C ile eşit. **`secure_delete`:** `ON` iken dosyada kalan artık sayısı C ile aynı (1.955; 1.932 canlı) — C bölünmeden sonra sayfa boşluğunda eski bir kopya bırakıyor, motor artık aynısını yapıyor. **Farksal koşular:** 4.000 tohum, çıktı C ile aynı, her dosya C `integrity_check` geçiyor; DML farksalı 377/400 (değişmedi). **Kurtarma eşliği:** üç kipte 99/100, 94/94, 99/100 ("her motor kendi dosyasını kurar" kipi önce 49/100 idi). **Çökme/geri alma:** 0 hata. **Bedel:** toplam 1,006x; komut sayıları `W08` 0,980, `W10` 0,946, `W13` 0,986, `W14` 0,992 (azaldı), `W01` 1,001, `W06` 1,006.
- **Toplam**: `249.87 ms` (taban `247.67 ms`, C `295.97 ms`) — geomean `Rust/C` **`0.820x`** (disk WAL `0.793x`, bellek-içi `0.928x`)
- **Tepe RSS**: `18,308 KiB` (C: `6,240 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.61`** | `7.52` | `7.42` | **`1.026x`** | `1.002x` | `9.71` | **`0.78x`** | `9,232` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.47`** | `7.44` | `7.49` | **`0.998x`** | `0.998x` | `8.69` | **`0.86x`** | `10,200` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.77`** | `27.49` | `27.61` | **`1.006x`** | `0.990x` | `23.28` | **`1.19x`** | `10,200` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.80`** | `7.68` | `7.72` | **`1.011x`** | `1.006x` | `7.43` | **`1.05x`** | `10,264` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.27` | `4.26` | **`1.004x`** | `0.998x` | `16.57` | **`0.26x`** | `10,264` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.72`** | `10.71` | `11.02` | **`0.973x`** | `0.997x` | `28.17` | **`0.38x`** | `10,392` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.89`** | `38.44` | `38.91` | **`1.000x`** | `1.004x` | `50.31` | **`0.77x`** | `12,764` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.55`** | `9.35` | `10.09` | **`0.946x`** | `1.001x` | `8.05` | **`1.19x`** | `12,892` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.91`** | `11.86` | `11.85` | **`1.005x`** | `0.999x` | `19.10` | **`0.62x`** | `12,892` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.18`** | `11.04` | `11.19` | **`0.999x`** | `0.996x` | `11.00` | **`1.02x`** | `13,076` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.74`** | `45.95` | `46.03` | **`1.015x`** | `1.031x` | `57.91` | **`0.81x`** | `18,268` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.37`** | `23.52` | `24.41` | **`0.999x`** | `1.042x` | `25.00` | **`0.97x`** | `18,308` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.94`** | `4.74` | `4.90` | **`1.009x`** | `0.960x` | `4.11` | **`1.20x`** | `18,340` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.64`** | `36.53` | `36.71` | **`0.998x`** | `1.005x` | `26.64` | **`1.38x`** | `18,308` | `fc7490e2b80c68e2` ✓ |
