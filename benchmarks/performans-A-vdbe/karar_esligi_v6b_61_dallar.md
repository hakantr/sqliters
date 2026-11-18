# V6b.61 — D165 C dal kapsamı

Üç profil sırasıyla 36 prepared UPDATE/DELETE RETURNING, 27 D52 INSERT ve 33 INSERT OR FAIL native vaka içeriyor. C `sqlite/src/vdbeaux.c:3496–3502` bağlantı sayacı kararları incelendi.

| C satırı | UPDATE/DELETE | INSERT | OR FAIL | Karar |
|---|---|---|---|---|
| 3496 | 72, 324 | 66, 168 | 69, 237 | Sayaç güncellemesini etkinleştiren DML ve diğer ifade kapanışları alındı. |
| 3497 | 72, 0 | 66, 0 | 69, 0 | Bu profillerde SAVEPOINT_ROLLBACK alt çıkışı alınmadı. Açık SQL ROLLBACK ayrı işlem olarak yürütüldü. |
| 3498 | 72 çağrı | 66 çağrı | 69 çağrı | İfade sayısı bağlantıya aktarıldı. |

Ham profiller `target/perf-work/d165-proof/final-worktree/res` altında. C koşulu ve Rust API sonucu ayrı ölçüldü.
