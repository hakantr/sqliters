# V6b.46 — D150 normal sıcak DELETE sonlandırması

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_46_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:2150–2159,3671–3692`, normal disk DELETE | Journal xClose çağrısının dönüşü sonlandırmayı durdurmaz. xDelete dizin sync bayrağı OFF, NORMAL ve FULL için 0, EXTRA için 1. Bu C referansında BATCH_ATOMIC_WRITE kapalı. | `finish_hot_journal` (`lib.rs:6660–6680`) normal DELETE için mevcut `remove_delete_journal` (`lib.rs:5726–5736`) çağrısını kullanır. xClose 4106 döndürdüğünde xDelete çalışır ve sorgu eski satırı döndürür. |
| `pager.c:2209–2217`, DELETE ve SHARED | xDelete hatasında da EXCLUSIVE kilit SHARED'a düşürülmeye çalışılır. İkisi hata verirse ilk finalizasyon hatası döner. xDelete başarılıysa SHARED hatası döner. | `play_hot_journal` (`lib.rs:6485–6496`) iki işlemi yapıp sonuçları sırasıyla denetler. Birleşik hatada 2570 döndü. Tek SHARED hatasında 2058 döndü. EXTRA dizin sync 1290 hatasında silinmiş journal yeniden oluşmadı. |
| `pager.c:5425–5443`, sıcak kurtarma hatası | Pager ERROR temizliği dosya kilidini tek NO_LOCK isteğiyle bırakır. Sonraki sorgu journal hâlâ duruyorsa tekrar kurtarır. | `recover_hot_journal_under_shared_lock` ve `begin_read` (`lib.rs:3112–3118,6369–6376`) normal DELETE'de hata temizliğini tek yerde yürütür. Native ilk sorgu hata kodu, NO_LOCK sırası, sağlam journal özeti ve ikinci sorgu eşit. Kalıcı Rust testi FULL ve EXTRA'da birleşik hatayı ve retry'ı kapsar. |
| `pager.c:3036–3046`, bağlı super-journal | Başarılı sonlandırma ve SHARED dönüşünden sonra super-journal incelenir. Önceki hata varsa super-journal açılmaz. | `lib.rs:6485–6498` SHARED'dan sonra `pager_delsuper` çağırır. Başarı durumunda açma ve silme sırası, DELETE ve SHARED hatalarında super-journal'ın disk üzerindeki hali eşit. |

Normal DELETE, dört synchronous kipi ve iki sorguyla 40 native vakada karşılaştırıldı. Ek 24 vaka 512, 4096, 8192 ve 65536 baytlık sayfalarda FULL ve EXTRA için başarı, kapatma hatası ve birleşik DELETE/SHARED hatasını kapsadı. Her iki Rust kipinde C ile aynı olay sırası, SQL sonucu ve dosya SHA-256 değerleri ölçüldü. Ham kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b50-proof/` altında. [Açık farklar](gorulen_farklar_v6b_46.md).

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1145 geçti, 7 başarısız, 337 yok sayıldı. ON 1131 geçti, 21 başarısız, 337 yok sayıldı. Yeni sıcak DELETE testi iki kipte geçti. Başarısız test kümeleri V6b.45 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.45 ile aynı. |
| Ayrışma bulucu | C ile 245 stdout ve 244 stderr sonucu eşit. V6b.45 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 297,47 ms, aday 371,67 ms. Aday/taban 1,249. 14 digest eşit. |

D149 başlık vakaları 203/203 eşit. Önceki 147 kurtarma vakası, 12 journal imajı ve dört gerçek C çöküş imajı tekrar eşit. Dört kapı ve kaynak SHA-256 kaydı `/home/hakan/github/sqliters/target/perf-work/v6b50_verify/res/` altında.
