# V6b.53 — D157 C dal kapsamı

SQLite 3.54.1'in `pager.c:1531–1539,4354–4435` kapsamı 40 dal çıkışı içeriyor. Cihaz bayrağı matrisinde 23, spill matrisinde 24 ve iki profil topluluğunda 27 çıkış alındı. Tabloda en az bir profilde alınmayan 20 çıkış yer alıyor.

Üçüncü, 32 vakalık FULLFSYNC profili aynı cihaz kapsamındaki `4425/0` çıkışını 12 kez, `4425/1` çıkışını sıfır kez aldı. Diğer çıkışların alınma durumu ilk cihaz profiliyle aynı. Üç profil birlikte 28/40 çıkışı kapsıyor.

| C satırı ve dal | Cihaz matrisi | Spill | Karar |
|---|---:|---:|---|
| 1532/1 | 8 | 0 | OFF sağlıklı disk işlemlerinde sentinel kullanıldı. Spill sync açık. |
| 1532/3 | 0 | 0 | MEMORY sentinel yolu disk matrisleri dışında. |
| 4354/1 | 0 | 0 | Açık journal bulunmayan sync yolu iki matriste de çağrılmadı. |
| 4354/3 | 0 | 0 | MEMORY sync cihaz I/O'sunu atlıyor. İki matris disk journal kullanıyor. |
| 4390/0 | 0 | 0 | Bir sonraki hizalı journal başlığının sekiz baytı okunamadı. D158'de ayrıca ölçülüyor. |
| 4390/2 | 0 | 0 | Başarıyla okunan sonraki başlıkta magic eşitliği ve bir bayt temizliği bu iki profilde yok. |
| 4390/3 | 0 | 0 | Tam okunan sonraki başlıkta magic uyuşmazlığı bu iki profilde yok. |
| 4394/1 | 0 | 0 | Tamamlanan stale başlık kontrolü bu matrislerde SHORT_READ ile döndü. |
| 4394/2 | 0 | 0 | SHORT_READ dışındaki tarama hatası bu matrislerde yok. |
| 4409/3 | 4 | 0 | SEQUENTIAL halinde FULL/EXTRA ön sync atlandı. Spill SEQUENTIAL kullanmıyor. |
| 4413/0 | 0 | 0 | Ön sync başarısızlığı iki sağlıklı profilde alınmadı. |
| 4419/0 | 0 | 0 | Başlık magic/nRec yazma başarısızlığı iki sağlıklı profilde alınmadı. |
| 4421/1 | 12 | 0 | SEQUENTIAL halinde son sync atlandı. Spill SEQUENTIAL kullanmıyor. |
| 4425/0 | 0 | 0 | Ek FULLFSYNC profili DATAONLY eklemeyi C ve iki Rust kipinde 12 kez aldı. |
| 4427/0 | 0 | 0 | Son sync başarısızlığı iki sağlıklı profilde alınmadı. |
| 4431/0 | 0 | 36 | Spill yeni bölüm istedi. Küçük COMMIT işlemleri istemedi. |
| 4431/2 | 0 | 18 | Spill, SAFE_APPEND kapalıyken yeni bölüm açtı. |
| 4431/3 | 0 | 18 | Spill, SAFE_APPEND açıkken yeni bölümü atladı. |
| 4434/0 | 0 | 0 | Yeni bölüm başlığı yazma hatası iki sağlıklı profilde yok. |
| 4434/1 | 0 | 18 | Spill'in açtığı yeni bölüm başlığı başarıyla yazıldı. |

Ham cihaz kapsamı `target/perf-work/v6b57-proof/final-worktree/cov/sqlite3.c.gcov`, spill kapsamı `target/perf-work/v6b57-proof/final-worktree/spill/cov/sqlite3.c.gcov` ve FULLFSYNC kapsamı `target/perf-work/v6b57-proof/fullfsync/cov/sqlite3.c.gcov` altında. Dal kimlikleri ve kaynak satırları önceki bağımsız C profiliyle ayrı karşılaştırıldı.
