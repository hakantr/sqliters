# V6b.62 — D166 C dal kapsamı

Üç yerel C profili sağlıklı kapanış, VFS yanıtı ve önceki Rust adapter'ının dosya sonucu için referans içeriyor. `wal.c:2597–2647` ve `pager.c:4239–4250` içinde kaydedilen 22 çıkışın 15'i bu profillerde alındı.

| C satırı ve dal | Alınmayan karar |
|---|---|
| pager.c 4243/1 | WAL kapanışında bağlantı nesnesi yok. |
| pager.c 4243/3 | Bağlantı son kapanış checkpoint'ini devre dışı bıraktı. |
| pager.c 4244/2 | DB kaydedilen konumunda bulunmadı veya VFS sorgusu hata verdi. |
| wal.c 2597/1 | Açık WAL nesnesi yok. |
| wal.c 2610/1 | Kapanış checkpoint tamponu yok. |
| wal.c 2613/1 | WAL normal kilit kipinin dışında. |
| wal.c 2619/1 | EXCLUSIVE aldıktan sonra kapanış checkpoint'i hata verdi. |

EXCLUSIVE başarı/BUSY sayıları üç profilde `6/3`, `7/0`, `3/0`. Sil/tut kararları `2/4`, `4/3`, `1/2`. Tutulan WAL için sıfırla/önceki boyutu koru kararları `2/2`, `1/2`, `1/1`. Ham C kapsamı ve ölçüm dizini `target/perf-work/d166-proof/final-worktree/res/measurement-path.txt` üzerinden izlenir.
