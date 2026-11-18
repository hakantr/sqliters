# V6b.63 — D167 WAL kapanışı öncesi dosya kimliği

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Bağımsız ölçüm `cf2635ef33ffeb0cd8dc51b4c8732b1914d99a17` üzerine uygulanan ve üç Rust dosyasını değiştiren `54c0cf9aacf4eebdb64eec6c41b8ef80228f6cec988409f16606d1bfade67fb5` SHA-256 özetli yamayı kullandı. Aday kaynakları, bağımsız ikililer ve yerel dört kapının kaynak kaydı teslimdeki manifestlerle doğrulandı.

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pager.c:4196–4213, 4243–4248` | Olağan disk kapanışında kimlik sorgusunun sağlıklı sonucu veya `SQLITE_NOTFOUND` checkpoint'e izin veriyor. Kimlik değişikliği veya VFS hatası checkpoint'i atlıyor. | `Pager::close` boş DB için erken izin ve diğer disk DB'leri için `VfsFile::has_moved` kararı. |
| `os_unix.c:1623–1630` | Açık dosyanın kimliği, kayıtlı yolun güncel kimliğiyle karşılaştırılıyor. Yol bilgisi alınamazsa değişmiş sonucu dönüyor. | Unix disk tanıtıcısı açılan inode ile yol inode'unu karşılaştırıyor. Varsayılan adapter `SQLITE_NOTFOUND` döndürüyor. |
| `wal.c:2597–2653` | Checkpoint kapalıysa EXCLUSIVE denemesi ve yan dosya sonlandırması atlanıyor. Açık tanıtıcılar kapanıyor. | `WalFile::close_durable` checkpoint kararını kullanıyor. Son DB kilidi ve tanıtıcısı terminal akışta bırakılıyor. |

Bağımsız gerçek kaynak karşılaştırmasında 18 sentetik `HAS_MOVED` yanıtı, 6 sentetik sağlıklı yanıt, 6 olağan kapanış, 7 `PERSIST_WAL` yanıtı ve 3 eski VFS adapter vakası iki Rust kipinde doğrulandı. Sentetik yanıtlar, mevcut veritabanı dosyaları üzerinde test VFS'inden dönüyor. Her fazda DB/WAL/SHM varlığı, boyutu ve SHA-256, kapanış sonucu ve C ile yeniden açılış karşılaştırıldı. Yeniden açılışta commit edilen değer `2` ve `integrity_check` sonucu `ok` okundu. Kapanışta silme veya tutma kararını etkin `PERSIST_WAL` değeri belirliyor. Rust odak testleri 10/10 geçti.

Ortak VFS olayları sırayla karşılaştırıldı. C'nin 55 `SHM_UNMAP` olayına karşı Rust tarafında bu olay ayrı görünmüyor. Üç eski adapter vakasında desteklenmeyen `PERSIST_WAL` çağrısı ayrıca doğrulandı ve ortak olay karşılaştırması bu adapter farkını dışarıda bıraktı. [Dal kapsamı](karar_esligi_v6b_63_dallar.md) · [Açık farklar](gorulen_farklar_v6b_63.md).

| Kapı | Sonuç |
|---|---|
| Çalışma alanı | OFF 1160 geçen, 7 başarısız, 337 yok sayılan. ON 1146 geçen, 21 başarısız, 337 yok sayılan. Başarısız kümeler V6b.62 ile aynı. |
| DML fuzzer | OFF 380/400, ON 400/400. Önceki ham özetler aynı. |
| Ayrışma bulucu | 245/245 stdout, 244/245 stderr. Önceki ham özet aynı. |
| Hız ve sonuç digest'i | 299,12 ms taban, 374,39 ms aday, 1,252 kat. 14/14 digest eşit. |

Önceki 147/147 kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküşü kaydı aynı aday manifestiyle doğrulandı. Ham ölçüm dizinleri `/home/hakan/github/sqliters/target/perf-work/d167-independent-proof/res` ve `/home/hakan/github/sqliters/target/perf-work/d167-verify/res`.
