# V6b.39 — D143 native modül kaydı ve hazırlama sırası

Native bağlantı C referansının beş başlangıç modülünü kaydeder. `PRAGMA module_list` hazırlamada kayıt tablosunun hash sırasını dondurur. JSON ve taşınmış PRAGMA sanal tabloları, bir kaynak gerçekten çözümlendiğinde kaydolur. Kabuk `generate_series` modülünü açılışta kaydeder. Tcl `load_static_extension db series` aynı uzantıyı açıkça yükler.

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [Alınan ve alınmayan C dalları](karar_esligi_v6b_39_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kapsam |
|---|---|---|
| `pragma.c:1489–1497`, `PRAGMA module_list` | Hazırlama sırasında `db->aModule` hash sırası VDBE satırlarına yazılır. | `VTabRegistry::module_names_in_hash_order`, `Host::pragma_module_names` ve `prepare_row_pragma`. Bir önceki hazırlanmış listenin sonradan yapılan kayıtla değişmemesi V1 ve V2 için denetlendi. |
| `vtab.c:39–78`, `hash.c:242–270`, kayıt, değiştirme, silme | Modül adı ve verisi hash tablosundan eklenir, değiştirilir veya silinir. Eşik aşılırsa tablo yeniden dağıtılır. | Gerçek modül kayıtları `register_module_named` ve `unregister_module` üzerinden mevcut `CHash` sırasını günceller. 96 gerçek kayıt ve yeniden kayıt denendi. Bağlı modülün değiştirilmesindeki eski veri farkı ayrıca kaydedildi. |
| `build.c:416–454`, `sqlite3LocateTable` | Önce şemadaki nesne aranır. Eponymous JSON ve PRAGMA modülü gerektiğinde kaydolur. | `lazy_module` kanonik JSON adını, PRAGMA için ilk çağrının yazımını kullanır. CREATE VIRTUAL TABLE var olan kayıt üzerinden ilerler. `pragma_cache_size` gerçek geri çağrıyla çalışır. |
| `select.c:5788–5833`, FROM kaynağı bir CTE'ye başvuruyor | En yakın WITH kapsamı seçilir. Kullanılmayan CTE'nin kaynakları bu yoldan çözülmez. | `CteScope` ve `ensure_qname_vtab_connected` görünür tanımı ilk başvuruda ziyaret eder. İleri başvuru, iç içe gölgeleme, özyineleme ve DML denetlendi. |
| `main.c:60–75` yerleşik uzantı başlatıcıları ve `shell.c.in:5243–5250` kabuk açılışı | Native ve kabuk farklı modül kümeleriyle açılır. Kabuk gerçek series uzantısını kaydeder. | Native başlangıç C'nin beş modülünü taşır. Kabuk ve Tcl gerçek `GenerateSeriesModule` kaydını kendi girişlerinde yapar. `fsdir` ve `completion` ayrı kabuk işi. |

46 native akışta çıkış kodu, stdout, stderr ve ana dosya hash'i aynı. V6b.38 ile bu akışlarda başlangıç modül listesi nedeniyle 0 tam eşleşme vardı. Kayıt geçmişinde liste sırası ve son dosya hash'i aynı. V6b.38'den kalan bağlı modül değiştirme farkı iki kipte de sürdü. [Açık farklar](gorulen_farklar_v6b_39.md).

Ham kanıt dizini `/home/hakan/github/sqliters/target/perf-work/v6b40/`. `module_cte_ext.py`, `history.c`, `history-driver`, `module_coverage.py`, `cli_modules.py`, `check_recheck.sh` ve `gates_recheck.sh` ölçümleri üretir. `res/source_manifest.json`, `res/oracle_and_binaries.json`, `res/validation.txt` ve `res/verified_commit.txt` gözlemleri sürüme bağlar.

Kabuk matrisinde önceki 2/12 yerine 4/12 akış eşit. Sekiz açık akış fsdir, completion ve bunlardan etkilenen iki liste sorgusuna ait. Bütün ana dosya hash'leri aynı. Kabuk karşılaştırma testleri series uzantısını açıkça kaydediyor. Testlerin beklenen çıktıları korundu.

| Yayın kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1140 geçti, 4 başarısız, 337 yok sayıldı. ON 1126 geçti, 18 başarısız, 337 yok sayıldı. Başarısız test kümeleri önceki sürümle aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları önceki sürümle aynı. |
| Ayrışma bulucu | 245 senaryonun stdout'u ve 244 senaryonun stderr'i C ile eşit. Önceki özet bayt düzeyinde aynı. |
| Rust 1.100 tabanına hız, yedi deneme | Toplam medyan taban 301,51 ms, aday 368,92 ms. Aday/taban 1,224. 14 digest eşit. |
