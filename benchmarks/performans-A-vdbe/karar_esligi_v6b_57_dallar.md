# V6b.57 — D52 C dal kapsamı

27 native işlem dizisi üç kodlama ve dokuz ifade yaşam döngüsünü kapsıyor. C kapsamı `sqlite/src/vdbeaux.c:3496–3502` bağlantı sayacı kararından alınır.

| C satırı | Dal sayaçları | Karar |
|---|---|---|
| 3496 | 66, 168 | Sayaç güncellemesi etkin DML ve diğer ifade kapanışları alındı. |
| 3497 | 66, 0 | Başarılı sayaç gönderimi 66 kez ölçüldü. SAVEPOINT_ROLLBACK alt dalı bu 27 başarılı native dizide alınmadı. |
| 3498 | 66 çağrı | İfade sayısı kapanışta bağlantıya aktarıldı. |

Genişletilmiş yaşam döngüsü ve aktif yazıcı profilinde bu karar 222, commit hook profilinde dört kez alındı. Ertelenmiş FK ve hook reddinde C'nin gözlenen sayacı ve Rust API sonucu eşit. Üç profilde bu koşulun SAVEPOINT_ROLLBACK alt çıkışı sıfır. Ham C kapsam dizini `target/perf-work/d52-proof/lifecycle/final-worktree/res/measurement-path.txt` ve ek kapsamlar `target/perf-work/d52-worktree-extra` altında.
