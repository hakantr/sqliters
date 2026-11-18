# V6b.64 — REGEXP hazırlamasında C dal kapsamı

SQLite 3.54.1'in bağımsız callback profilinde C `resolve.c:1166–1178` ve `1308–1327` aralıkları ölçüldü. Toplam 16 dal çıkışından 10'u alındı. 138 kesin arite aramasında 114 sonuç bulundu. 24 ikinci arama 12 bilinmeyen fonksiyon ve 12 yanlış arite tanısına ayrıldı.

| C satırı | Alınmayan çıkış |
|---|---|
| `1167/1` | Argüman listesi NULL. Bu profilde çağrılar argüman taşıyor. |
| `1309/0` | Şema init durumu etkin. |
| `1309/2` | Şema init etkinken `no_such_func==2` alt koşulunun ilk çıkışı. |
| `1309/3` | Şema init etkinken aynı alt koşulun diğer çıkışı. |
| `1310/0` | İç ALTER TABLE şema kipini karşılaştıran son koşulun ilk çıkışı. |
| `1310/1` | İç ALTER TABLE şema kipini karşılaştıran son koşulun diğer çıkışı. |

Son beş çıkış, olağan SELECT hazırlamasında etkin olmayan şema init yolunda. `SQLITE_ENABLE_UNKNOWN_SQL_FUNCTION` derleme seçeneği bu Oracle profilinde etkin değil. Ham sayaç `/home/hakan/github/sqliters/target/perf-work/regexp-prepare-proof/resolver-gcov/resolve-scope.txt`.
