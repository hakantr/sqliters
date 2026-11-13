# `performans-A-hizalama` — C'ye Hizalama Adımları (Bedelli) Ölçüm Raporu

- **Donanım / İS**: `Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz` (56 iş parçacığı), `Linux 7.0.0-38-generic (x86_64)`
- **Rust Derleyicisi**: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` — `release (opt-level=3, lto=fat, codegen-units=1, strip=symbols)`
- **C SQLite Referansı**: `cc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` (SQLite `3.54.1`, `-O3 -DNDEBUG -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_MATH_FUNCTIONS`)
- **Ölçüm zamanı**: `2026-11-13T14:31:46+03:00`
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
| **C SQLite 3.54.1 (`-O3`)** | `referans` | `2,298.0` | `1,771.8` | **`293.85`** | `—` | **`1.000x`** | **`1.000x`** | `6,380` | `—` |
| **00_performans_A_ucu** | `performans-A@225f5c2` | `7,299.0` | `6,157.8` | **`247.86`** | `1.000x` | **`0.824x`** | `0.843x` | `18,144` | 14/14 ✓ |
| **01_h1_silmede_dengeleme** | `performans-A-hizalama@034af4f` | `7,346.9` | `6,193.5` | **`248.11`** | `0.999x` | **`0.824x`** | `0.844x` | `18,416` | 14/14 ✓ |
| **02_h1b_dml_stratejisi** | `performans-A-hizalama@ebc3fde` | `7,461.8` | `6,294.8` | **`249.27`** | `1.002x` | **`0.826x`** | `0.848x` | `18,472` | 14/14 ✓ |

## 2. İş Yükü Matrisi (medyan `ms`, parantez içinde `Rust / C`)

| İş Yükü | C `-O3` | 00_performans_A_ucu | 01_h1_silmede_dengeleme | 02_h1b_dml_stratejisi |
| :--- | ---: | ---: | ---: | ---: |
| `W01_bulk_insert_prepared` | `9.64` | `7.58` (**0.79x**) | `7.56` (**0.79x**) | `7.41` (**0.77x**) |
| `W02_create_indexes` | `8.65` | `7.48` (**0.86x**) | `7.55` (**0.87x**) | `7.53` (**0.87x**) |
| `W03_pk_point_select` | `23.17` | `27.79` (**1.20x**) | `27.78` (**1.20x**) | `28.06` (**1.21x**) |
| `W04_secondary_idx_range` | `7.38` | `7.85` (**1.06x**) | `7.80` (**1.06x**) | `7.85` (**1.06x**) |
| `W05_full_scan_filter_agg` | `16.58` | `4.28` (**0.26x**) | `4.38` (**0.26x**) | `4.27` (**0.26x**) |
| `W06_join_group_order` | `28.23` | `10.74` (**0.38x**) | `10.63` (**0.38x**) | `11.01` (**0.39x**) |
| `W07_window_functions` | `50.14` | `38.80` (**0.77x**) | `38.54` (**0.77x**) | `38.74` (**0.77x**) |
| `W08_indexed_update_delete` | `7.95` | `9.60` (**1.21x**) | `9.86` (**1.24x**) | `9.86` (**1.24x**) |
| `W09_like_text_scan` | `19.00` | `12.14` (**0.64x**) | `11.93` (**0.63x**) | `11.83` (**0.62x**) |
| `W10_jsonb_extract_agg` | `10.89` | `11.27` (**1.03x**) | `11.13` (**1.02x**) | `11.19` (**1.03x**) |
| `W11_fts5_index_and_bm25` | `57.17` | `45.26` (**0.79x**) | `46.25` (**0.81x**) | `45.98` (**0.80x**) |
| `W12_rtree_insert_and_bbox` | `24.43` | `23.64` (**0.97x**) | `23.64` (**0.97x**) | `24.28` (**0.99x**) |
| `W13_autocommit_wal_txns` | `4.07` | `4.89` (**1.20x**) | `4.75` (**1.17x**) | `4.82` (**1.18x**) |
| `W14_cli_sql_script` | `26.55` | `36.54` (**1.38x**) | `36.30` (**1.37x**) | `36.45` (**1.37x**) |
| **Toplam** | **`293.85`** | **`247.86`** (**0.84x**) | **`248.11`** (**0.84x**) | **`249.27`** (**0.85x**) |

---

## 00_performans_A_ucu — `performans-A@225f5c2`

**Baz.** `performans-A` dalının bu dal ayrılırken ucu (adım 5j). Ayrışma bulucu, 234 senaryo, son dosya katmanı: L0 56, L2 43, L3 3, L4 58, L5 59, L6 15.

- **Testler**: `cargo test --workspace`: 757 geçti, bilinen 4 başarısız, 337 yok sayıldı.
- **Toplam**: `247.86 ms` (taban `247.86 ms`, C `293.85 ms`) — geomean `Rust/C` **`0.824x`** (disk WAL `0.799x`, bellek-içi `0.926x`)
- **Tepe RSS**: `18,144 KiB` (C: `6,380 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.58`** | `7.49` | `1.000x` | `9.64` | **`0.79x`** | `9,132` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.48`** | `7.41` | `1.000x` | `8.65` | **`0.86x`** | `9,972` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.79`** | `27.67` | `1.000x` | `23.17` | **`1.20x`** | `9,972` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.85`** | `7.75` | `1.000x` | `7.38` | **`1.06x`** | `10,036` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.28`** | `4.26` | `1.000x` | `16.58` | **`0.26x`** | `10,036` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.74`** | `10.70` | `1.000x` | `28.23` | **`0.38x`** | `10,100` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.80`** | `38.39` | `1.000x` | `50.14` | **`0.77x`** | `12,524` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.60`** | `9.51` | `1.000x` | `7.95` | **`1.21x`** | `12,524` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`12.14`** | `11.81` | `1.000x` | `19.00` | **`0.64x`** | `12,524` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.27`** | `11.20` | `1.000x` | `10.89` | **`1.03x`** | `12,772` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.26`** | `44.89` | `1.000x` | `57.17` | **`0.79x`** | `18,016` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.64`** | `23.17` | `1.000x` | `24.43` | **`0.97x`** | `18,036` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.89`** | `4.77` | `1.000x` | `4.07` | **`1.20x`** | `18,068` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.54`** | `36.37` | `1.000x` | `26.55` | **`1.38x`** | `18,036` | `fc7490e2b80c68e2` ✓ |

---

## 01_h1_silmede_dengeleme — `performans-A-hizalama@034af4f`

**H1 — silmede C gibi yeniden dengeleme.** C, bir silmeden sonra sayfanın boş alanı kullanılabilir alanın üçte ikisini aşarsa `balance()` çağırır ve `balance_nonroot()` hücreleri en çok üç kardeş arasında yeniden dağıtır (gerekirse bir sayfayı serbest bırakır, kökte bir seviye düşürür); iç sayfadaki bir hücre silinince yerine öncül yaprak hücresi konur. Motor yalnızca yaprak tamamen boşaldığında davranıyordu; iç indeks hücresi silmede bazı durumlarda tüm indeksi yeniden kuruyordu. Yeni `sqliters-btree/src/balance.rs`, C'nin `balance` / `balance_nonroot` / `balance_deeper` / `balance_quick`, `insertCell` / `dropCell` / `allocateSpace` / `defragmentPage`, `editPage` / `rebuildPage` işlevlerinin portudur; tüm silme yolları (genel, konumlu, ham hızlı yollar, indeks) buna bağlandı. Hızlı yollar ham kaldı: üçte iki testi O(1) bir sınırla başlıyor. Ekleme tarafı bu adımda değişmedi (H2 aynı kodu etkinleştirecek). Motor düzeyinde bir düzeltme de gerekti: satır kimliği çakışmasında `REPLACE` artık C gibi yalnızca indeks girdilerini silip satırın üzerine yazıyor.

- **Testler**: `cargo test --workspace`: 764 geçti (7 yeni), bilinen 4 başarısız, 337 yok sayıldı. **Karar eşliği** (iki motor aynı başlangıç dosyasından; 95 senaryo, 762.748 C olayı): 93 senaryoda her karar, SQL çıktısı, bütünlük ve sayfa sayfa şekil C ile aynı; kalan ikisi bu adımdan önce de var olan motor düzeyi farklar (indeks taramalı silmede ağaçlar arası yazma sırası; tetikleyici içi `DELETE`in satır ziyaret sırası). Hiç tetiklenmeyen kararlar: `balance_quick` ve `bBulk` dalları (yalnızca eklemede), bozuk girdi dalları. Ayrıntı: `karar_esligi_h1.md`. **Ayrışma bulucu** (245 senaryo, son derleme): kötüleşen yok, 24 iyileşti (L5 59 → 39, L2 44 → 55). **Sayfa sayıları (motor/C):** karışık 4096: 296 → 251 / 249; 512: 2456 → 2214 / 2173; 1024 + auto_vacuum: 1182 → 986 / 972; WAL: 281 → 244 / 244. Kalan fark ekleme tarafı bölünmelerden (H2). **Farksal koşu:** 4.800 tohum, çıktı C ile ve dal noktasıyla aynı, her dosya C `integrity_check` geçiyor. **Çökme/geri alma:** 288 betik + commit sırasında öldürülen 2.660 kurtarma, 0 hata. **Kurtarma eşliği (kapı tam karşılanmadı):** otomatik commit'te aynı şekilli 93 dosyanın 93'ünde C `.recover` çıktısı aynı; açık işlem içinde 99'un 84'ünde. Neden bu adımdan önce de var: motor açık işlemdeki her genel-yol ifadesi için ifade kayıt noktası açıyor (C yalnızca çok satırlı ve iptal olabilecek ifadelerde), bu yüzden serbest kalan sayfanın yazımı düşürülemiyor. **Bedel:** `W08` süre +%5,0 (ajanın koşusunda +%3,3), komut +%0,75; diğer iş yüklerinde komut sayısı aynı. Kalan maliyet: C sayfa başına boş alanı önbellekte tutuyor (`nFree`), port gerektiğinde yeniden hesaplıyor.
- **Toplam**: `248.11 ms` (taban `247.86 ms`, C `293.85 ms`) — geomean `Rust/C` **`0.824x`** (disk WAL `0.797x`, bellek-içi `0.928x`)
- **Tepe RSS**: `18,416 KiB` (C: `6,380 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.56`** | `7.45` | `7.58` | **`0.997x`** | `0.997x` | `9.64` | **`0.79x`** | `9,120` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.55`** | `7.45` | `7.48` | **`1.009x`** | `1.009x` | `8.65` | **`0.87x`** | `9,952` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`27.78`** | `27.50` | `27.79` | **`1.000x`** | `1.000x` | `23.17` | **`1.20x`** | `9,952` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.80`** | `7.70` | `7.85` | **`0.994x`** | `0.994x` | `7.38` | **`1.06x`** | `10,016` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.38`** | `4.35` | `4.28` | **`1.024x`** | `1.024x` | `16.58` | **`0.26x`** | `10,016` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`10.63`** | `10.59` | `10.74` | **`0.990x`** | `0.990x` | `28.23` | **`0.38x`** | `10,148` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.54`** | `38.18` | `38.80` | **`0.993x`** | `0.993x` | `50.14` | **`0.77x`** | `12,640` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.86`** | `9.79` | `9.60` | **`1.026x`** | `1.026x` | `7.95` | **`1.24x`** | `12,640` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.93`** | `11.84` | `12.14` | **`0.983x`** | `0.983x` | `19.00` | **`0.63x`** | `12,640` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.13`** | `11.07` | `11.27` | **`0.988x`** | `0.988x` | `10.89` | **`1.02x`** | `12,884` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`46.25`** | `45.22` | `45.26` | **`1.022x`** | `1.022x` | `57.17` | **`0.81x`** | `18,236` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`23.64`** | `23.31` | `23.64` | **`1.000x`** | `1.000x` | `24.43` | **`0.97x`** | `18,276` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.75`** | `4.58` | `4.89` | **`0.972x`** | `0.972x` | `4.07` | **`1.17x`** | `18,308` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.30`** | `35.89` | `36.54` | **`0.993x`** | `0.993x` | `26.55` | **`1.37x`** | `16,164` | `fc7490e2b80c68e2` ✓ |

---

## 02_h1b_dml_stratejisi — `performans-A-hizalama@ebc3fde`

**H1b — DML ifadelerinin C'nin kod üreticisi gibi yürütülmesi.** (A) **Geçiş stratejisi ve satır ziyaret sırası**: `DELETE` ve `UPDATE` için C'nin kararları port edildi (tek geçiş / satır kümesi / geçici indeks; `bComplex`, `chngKey`, `hasFK`, kısaltma optimizasyonu koşulları); iki geçişte satırlar C gibi satır kimliği/birincil anahtar sırasıyla işleniyor. Tetikleyicili tabloda indeks üzerinden `DELETE` artık C ile aynı sonucu veriyor (önce sonuç farklıydı). `AUTOINCREMENT` ifade başına sayaç kullanıyor; `last_insert_rowid` ve `total_changes` başarısız ifadede C gibi geri alınmıyor; tek satırlık `INSERT`te eksik üst satır hemen hata veriyor. (B) **Kısıt kontrol sırası ve yazma sırası**: NOT NULL → CHECK → satır kimliği → UNIQUE indeksler (C'nin indeks sırasıyla; önce ters sıradaydı); `STRICT` tür kontrolü C'nin konumunda; `REPLACE` çakışan indeks girdisini en son siliyor; üretilmiş sütunlar bağımlılık sırasıyla hesaplanıyor (ileri başvuruda yanlış indeks girdisi yazılıyor ve C `integrity_check` düşüyordu). (C) **İfade günlüğü** yalnızca C'nin açtığı durumda açılıyor (`isMultiWrite && mayAbort`); başarısız ifadenin neyi geri aldığı `sqlite3VdbeHalt` kurallarına bağlandı. Bir entegrasyon testi beklentisi C'ye göre değişti (başarısız commit'ten sonra `last_insert_rowid`).

- **Testler**: `cargo test --workspace`: 781 geçti (17 yeni), bilinen 4 başarısız, 337 yok sayıldı. **DML farksalı** (rastgele şema, tetikleyici, yabancı anahtar, çakışma yan tümceleri; CLI + API; her ifadeden sonra `changes`/`total_changes`/`last_insert_rowid`): ajanın 3.000 tohumunda C ile birebir aynı 2.839; bağımsız 400 yeni tohumda (yalnız CLI) **377** — H1 ve dal noktası 134. Dal noktasında görülen 2 C `integrity_check` hatası kalmadı. Kalan farklar (örnekle sınıflandı): SELECT planlayıcısından gelen satır sırası (görünümler, WITHOUT ROWID indeks seçimi), DDL hata metinleri, iki yabancı anahtar ayrıntısı, `INSERT…SELECT`in eş-yordam durumu. **Karar izi** (600 tohum, 6.487 ifade): günlük açma 0 uyuşmazlık, durdurma sonucu 0, strateji 28, günlük bayrakları 9; B-tree yazma dizisi 6.184 eşit / 303 farklı. **Kurtarma eşliği:** açık işlem içinde 99/100 aynı (H1: 84/99); kalan durum serbest sayfa yolu farkı. **H1 karar izi:** 95/95 tam eşit (H1: 93). **Ayrışma bulucu:** dağılım aynı, kötüleşen yok. **Çökme/geri alma:** yeni ifade-iptali takımı 84 betik + 432 kurtarma, diğer iki takım hızlı kipte, 0 hata. **Bedel:** toplam 1,007x; komut sayıları 0,999–1,001, yalnız `W12` +%0,9 ve `W11` +%0,3 (ifade başına günlük politikası çağrısı). `W06` +%3,8 süre aynı komut sayısıyla (kod yerleşimi). **Hizalanmayan:** WITHOUT ROWID `UPDATE` tüm indeks girdilerini siliyor/yeniden ekliyor (C yalnız değişenleri); UPSERT'te silme sırası; C'nin eş-yordamlı `INSERT…SELECT`i; DDL/ANALYZE/PRAGMA ifade günlüğü bayrakları; 9 bayrak uyuşmazlığının ~5'inde C'nin günlük açtığı yerde motor açmıyor (farksalda gözlenmedi, dışlanmadı).
- **Toplam**: `249.27 ms` (taban `247.86 ms`, C `293.85 ms`) — geomean `Rust/C` **`0.826x`** (disk WAL `0.799x`, bellek-içi `0.936x`)
- **Tepe RSS**: `18,472 KiB` (C: `6,380 KiB`)

| İş Yükü | Depolama | Rust (`ms`) | En düşük (`ms`) | Önceki aşama (`ms`) | `Bu / Önceki` | `Bu / Taban` | C `-O3` (`ms`) | `Rust / C` | Rust RSS (`KiB`) | FNV-1a |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `W01_bulk_insert_prepared` | `disk_wal` | **`7.41`** | `7.40` | `7.56` | **`0.980x`** | `0.978x` | `9.64` | **`0.77x`** | `9,280` | `cf7ef6490b505004` ✓ |
| `W02_create_indexes` | `disk_wal` | **`7.53`** | `7.45` | `7.55` | **`0.998x`** | `1.007x` | `8.65` | **`0.87x`** | `10,116` | `d812b7af65788b97` ✓ |
| `W03_pk_point_select` | `disk_wal` | **`28.06`** | `27.51` | `27.78` | **`1.010x`** | `1.010x` | `23.17` | **`1.21x`** | `10,116` | `5f466d818d90c0bd` ✓ |
| `W04_secondary_idx_range` | `disk_wal` | **`7.85`** | `7.71` | `7.80` | **`1.006x`** | `1.000x` | `7.38` | **`1.06x`** | `10,180` | `902cd404f2b91383` ✓ |
| `W05_full_scan_filter_agg` | `disk_wal` | **`4.27`** | `4.25` | `4.38` | **`0.975x`** | `0.998x` | `16.58` | **`0.26x`** | `10,180` | `44608b554d6b1493` ✓ |
| `W06_join_group_order` | `disk_wal` | **`11.01`** | `10.97` | `10.63` | **`1.036x`** | `1.025x` | `28.23` | **`0.39x`** | `10,244` | `fda96b550d88074f` ✓ |
| `W07_window_functions` | `disk_wal` | **`38.74`** | `38.40` | `38.54` | **`1.005x`** | `0.998x` | `50.14` | **`0.77x`** | `12,736` | `cda431c8e20d1408` ✓ |
| `W08_indexed_update_delete` | `disk_wal` | **`9.86`** | `9.77` | `9.86` | **`1.001x`** | `1.027x` | `7.95` | **`1.24x`** | `12,736` | `1ec6462bfbcb1629` ✓ |
| `W09_like_text_scan` | `disk_wal` | **`11.83`** | `11.80` | `11.93` | **`0.991x`** | `0.975x` | `19.00` | **`0.62x`** | `12,736` | `b7a41e6d0b21a8a5` ✓ |
| `W10_jsonb_extract_agg` | `memory` | **`11.19`** | `11.13` | `11.13` | **`1.005x`** | `0.992x` | `10.89` | **`1.03x`** | `12,920` | `92ab122fcccddb22` ✓ |
| `W11_fts5_index_and_bm25` | `memory` | **`45.98`** | `45.50` | `46.25` | **`0.994x`** | `1.016x` | `57.17` | **`0.80x`** | `18,204` | `fe20e26c772a3629` ✓ |
| `W12_rtree_insert_and_bbox` | `memory` | **`24.28`** | `23.44` | `23.64` | **`1.027x`** | `1.027x` | `24.43` | **`0.99x`** | `18,252` | `b123c883941db5cf` ✓ |
| `W13_autocommit_wal_txns` | `disk_wal` | **`4.82`** | `4.55` | `4.75` | **`1.014x`** | `0.985x` | `4.07` | **`1.18x`** | `18,284` | `afefef885327e87a` ✓ |
| `W14_cli_sql_script` | `disk_wal_cli` | **`36.45`** | `36.28` | `36.30` | **`1.004x`** | `0.997x` | `26.55` | **`1.37x`** | `18,272` | `fc7490e2b80c68e2` ✓ |
