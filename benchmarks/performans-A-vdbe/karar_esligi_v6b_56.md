# V6b.56 — D160 EXCLUSIVE rollback COMMIT phase-two bildirimi

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_56_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pager.c:2204–2217`, gerçek disk COMMIT | Journal sonlandırması başarılıysa normal ve EXCLUSIVE kilit kipinde COMMIT_PHASETWO gönderilir. SHARED kararı ayrı koşuldur. | `sqliters-pager/src/lib.rs:5418–5449` normal ve EXCLUSIVE disk rollback bildirimi için ayrı koşul kullanır. |
| `pager.c:6790–6797`, yazılmamış EXCLUSIVE PERSIST | `PAGER_WRITER_LOCKED` ve EXCLUSIVE/PERSIST özel dönüşü bildirimi atlar. | `lib.rs:5421–5428` journal'da değişiklik olmayan EXCLUSIVE PERSIST işlemini bildirim kapsamı dışında tutar. Kalıcı test art arda gerçek ve boş COMMIT'i denetler. |
| `pager.c:2206,2217`, bildirim sonucu | NOTFOUND başarıya çevrilir. Diğer ilk hata tek dosya COMMIT sonucunda korunur ve kalıcı veri okunabilir. | `lib.rs:5434–5464` aynı bildirimi ve sonuç önceliğini kullanır. |
| `pager.c:6798–6801`, `vdbeaux.c:3160–3179`, super-journal silindikten sonra | Çok dosyalı commit noktasından sonra katılımcı bildiriminin G/Ç hatası nihai COMMIT'i geri almaz. Sonraki katılımcı sonlandırılır. | Mevcut benign phase-two temizliği COMMIT noktasından sonra aynı sonucu verir. Sekiz çok dosyalı callback hata vakası bu karar için ölçülür. |

Native matris tek dosyalı ve iki katılımcılı COMMIT, DELETE/PERSIST/TRUNCATE, NORMAL/FULL, yazılmamış EXCLUSIVE PERSIST ve bildirim dönüşlerini kapsıyor. Ham soğuk journal dosyaları ayrıca kaydediliyor. [Açık farklar](gorulen_farklar_v6b_56.md).

Gerçek worktree'de 56/56 vakada bildirim, COMMIT, sonraki sorgu ve yeniden açılış, bütünlük kontrolü ve DB özetleri iki Rust kipinde C ile eşit. Tek DB bildirim hatasında COMMIT hata verdi ve veri korundu. Çok dosyalı COMMIT sekiz sonlandırma hatası vakasında tamamlandı. 120 journal görüntüsü ayrıca incelendi. Ham dosya kümesi eşliği 46/56. On PERSIST vakasındaki farklar bağımsız nonce'a bağlı checksum alanlarıyla sınırlı.

Ham native kayıtlar ve kaynak özetleri `target/perf-work/v6b60-proof/final-worktree/res` altında.

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1153 geçti, 7 başarısız, 337 yok sayıldı. ON 1139 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.55 ile aynı. Yeni D160 testi, D159 ve D155 regresyonları iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.55 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.55 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 297,80 ms, aday 370,29 ms, süre oranı 1,243.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/v6b60_verify/res` altında.
