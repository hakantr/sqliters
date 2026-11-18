# V6b.54 — D158 C dal kapsamı

SQLite 3.54.1'in `pager.c:4388–4396` kararları doğal EOF ve ek journal matrislerinde ayrı kaydedildi.

| C satırı ve dal | Doğal EOF | Ek journal | Karar |
|---|---:|---:|---|
| 4390/0 | 0 | 6 | Tam okuma. Altı ek vaka önceki başarılı PERSIST işleminden dosya kullanıyor. |
| 4390/1 | 16 | 6 | Tamamlanmayan okuma magic karşılaştırmasını atlıyor. |
| 4390/2 | 0 | 6 | Eşleşen magic bir bayt güncelleniyor. |
| 4390/3 | 0 | 0 | Tam fakat farklı magic, ayrıca ölçülen üç PERSIST işleminde alındı. |
| 4394/0 | 16 | 10 | Sonuç SHORT_READ olasılığı için denetleniyor. |
| 4394/1 | 0 | 2 | Başarılı okuma veya yazma ek hata karşılaştırmasını atlıyor. |
| 4394/2 | 0 | 4 | Başka hata ilk koduyla geri dönüyor. |
| 4394/3 | 16 | 6 | SHORT_READ sonrasında sync akışı devam ediyor. |

Doğal profil 3/8 ve ek profil 7/8 çıkış aldı. Farklı magic kullanılan üçüncü profil, `4390/3` çıkışını üç kez aldı. Üç profil birlikte 8/8. Sync OFF ve SAFE_APPEND kontrolleri bu taramayı atladı. Ham kapsam dosyalarının dizini `target/perf-work/v6b58-proof/final-worktree/res/measurement-path.txt` içinde kayıtlı. Üçüncü profil `target/perf-work/v6b58-proof/final-worktree/nonmagic-final-cov/sqlite3.c.gcov` altında.
