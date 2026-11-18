# V6b.55 — D159 C dal kapsamı

16 C işleminin her biri önce WAL'a geçiş hazırlığındaki rollback commit'i ve ardından ölçülen WAL COMMIT'i yürütür. Bu nedenle ortak pager bildirim kararları hem hazırlık hem ölçüm çağrılarını içerir.

| C konumu | Dal sayaçları | Gözlenen karar |
|---|---|---|
| `pager.c:2186` | 16, 16 | WAL ve hazırlıktaki rollback sonlandırması. |
| `pager.c:2204` | 32, 0, 32, 0 | Başarılı phase-two başlangıcı bildirime ulaştı. Önceki işlem hatası ve bCommit kapalı ayrımları bu COMMIT profili dışında. |
| `pager.c:2206` | 24, 8 | NOTFOUND olağan başarıya çevrildi, diğer sonuçlar korundu. |
| `pager.c:2209` | 32, 0 | Normal kilit kipi yolu alındı. EXCLUSIVE işlemler bu matrise dahil edilmedi. |
| `pager.c:2210` | 16, 16, 0, 16 | Normal WAL ve rollback kararları alındı. WAL exclusive kipinden çıkarken ana DB kilidinin bırakılması ölçülmedi. |
| `pager.c:2217` | 28, 4 | Başarı ve bildirim hatası. |
| `wal.c:3839` | 16, 116 | Bu işlemlerde etkin writer 16 kez bırakıldı. Diğer doğal kontrol çağrıları etkin writer görmedi. |

Ham C kapsamının dizini `target/perf-work/v6b59-proof/final-worktree/res/measurement-path.txt` içinde kayıtlı.
