# V6b.59 — D163 C dal kapsamı

`sqlite/src/pager.c:2460–2467` için 40 ana, 35 ek ve sekiz guard çalışması ayrı kaydedildi. 18 dal çıkışının 11'i bu profillerde alındı.

| C satırı ve dal | Alınmayan karar |
|---|---|
| 2460/1 | Ana journal yerine subjournal kaydı işlendiğinde alınır. |
| 2463/0 | Subjournal kaydında cache sayfası var. |
| 2463/1 | Subjournal kaydında cache sayfası yok. |
| 2463/2 | Subjournal sayfasında NEED_SYNC yok. |
| 2463/3 | Subjournal sayfasında NEED_SYNC var. |
| 2465/1 | Geri yazma kararında ana DB tanıtıcısı kapalı. |
| 2466/2 | Ana DB yazımına PAGER_OPEN üzerinden ulaşılır. Sıcak playback bu dar profilin dışında. |

`2461` sync sınırı kararı üç profilde `228, 78, 208, 20` olarak kaydedildi. `2467` geri yazma kapısında `283, 6` sonucu alındı. Önceki altı ek yazım kararının karşılığı bu ikinci çıkıştır. Ham sayaç ve kaynak satırları `target/perf-work/d163-proof/final-worktree/res/verify.txt` altında.
