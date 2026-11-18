# V6b.56 — D160 C dal kapsamı

56 gerçek C vakasında `pager_end_transaction` ve `sqlite3PagerCommitPhaseTwo` içindeki bildirim ve boş EXCLUSIVE/PERSIST kararları ölçüldü. Çalışmaların hazırlık, COMMIT ve temizliği bu ortak dalları birden çok kez kullanır.

| C satırı | Dal sayaçları | Karar |
|---|---|---|
| 2204 | 168, 0, 76, 92 | COMMIT ile sonlandırma bildirimi ve rollback ayrımları alındı. Bildirimden önceki phase-two sonlandırma hatası bu profilde uygulanmadı. |
| 2206 | 56, 20 | NOTFOUND başarıya çevrilir ve diğer sonuçlar korunur. |
| 2209 | 112, 56 | Normal ve EXCLUSIVE kilit seçimleri alındı. |
| 2217 | 154, 14 | Başarılı sonlandırma ile ilk hata çıkışı alındı. |
| 6790 | 14, 70 | Boş ve gerçek yazımlar ayırt edildi. |
| 6791 | 12, 2 | Boş EXCLUSIVE ve boş normal kilit akışları alındı. |
| 6792 | 8, 4 | Boş EXCLUSIVE PERSIST ve diğer kipler alındı. |
| 6796 | 8 erken dönüş | Boş EXCLUSIVE PERSIST COMMIT bildirimi atlandı. |

Ham C kapsam dizini `target/perf-work/v6b60-proof/final-worktree/res/measurement-path.txt` içinde kayıtlı.
