# V6b.60 — D164 C dal kapsamı

184 geniş native işlem, 16 odak V1/V2 vaka ve ayrı yaşam döngüsü koşusu ölçüldü. Aşağıda kapsamın dört karar satırı ve kaydedilen dal sayaçları var.

| C konumu | Geniş matris | Odak V1/V2 | Yaşam döngüsü | Koşul |
|---|---|---|---|---|
| `vdbeapi.c:951` | 16, 0, 8, 8 | 16, 0, 8, 8 | 4, 0, 2, 2 | Hata aktarımında V1 ve V2 ayrımı. DONE ile bu koşula gelen çıkış profilin üçünde de alınmadı. |
| `build.c:1053` | 16, 1048 | 16, 80 | 6, 2 | Bilinen ve bilinmeyen şema seçimi. |
| `pragma.c:3012` | 144, 16 | 16, 0 | 5, 0 | Şema nitelemesi. Odak ve yaşam döngüsü iç PRAGMA örnekleri şema adı verdi. |
| `pragma.c:3023` | 16, 144 | 16, 0 | 4, 1 | İç hazırlama hatası ve başarı. Odak matrisinin tamamı bilinmeyen şema. |

Bu dört satırda üç profil birlikte 10 çıkıştan dokuzunu aldı. Ham C kapsam dizini `target/perf-work/v1-locking-proof/final-worktree/res/measurement-path.txt` içinde kayıtlı.
