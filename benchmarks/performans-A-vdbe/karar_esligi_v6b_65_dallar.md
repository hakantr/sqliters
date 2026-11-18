# V6b.65 — C dal kapsamı

## REGEXP

| C aralığı | Alınan çıkış ve ölçüm |
|---|---|
| `parse.y:1370–1377, 1380–1388` | İki aritede 6/8 çıkış. Her aritede NOT 96, pozitif 192. |
| `vdbe.c:2726–2735` | OP_Not 2/2 çıkış. 168 değer, 24 NULL. |
| `vdbe.c:2819–2825` | OP_IfNot 2/2 çıkış. |
| `vdbe.c:9027–9078` | Function/PureFunc ortak gövdede 4/10 çıkış, 576 callback. 12 normal EXPLAIN kaydı OP_Function üretti. |

| Alınmayan C çıkışı | Sınıflandırma |
|---|---|
| `parse.y:1377/1` ve `1388/1` | Her aritede hazırlanmış ifade düğümü NULL. |
| `vdbe.c:9040/1` | Bağlam aynı çıktı kayıtçısını kullanıyor. |
| `vdbe.c:9060/0` | Callback hata veya auxdata temizleme akışına girdi. |
| `vdbe.c:9061/0` ve `9061/1` | Pozitif hata kodu denetiminin iki çıkışı. |
| `vdbe.c:9067/0` ve `9067/1` | Callback sonrasında abort kodu denetiminin iki çıkışı. |

Kesin satır ve sayaç kaynağı `/home/hakan/github/sqliters/target/perf-work/regexp-prepare-proof/value-parity/res/value-gcov-scope.txt`.

## Bellek ve disk mmap_size

`pragma.c:951–977` aralığında 50 geçişte 18 setter ve 32 getter ölçüldü. 30 file-control OK ve 20 NOTFOUND. Scope içinde 18 dal çıkışından 16'sı alındı. `pragma.c:958` negatif limit ve `pragma.c:974` NOTFOUND dışındaki G/Ç hata çıkışı bu profilde alınmadı. `SQLITE_MAX_MMAP_SIZE=0` ayrı derleme kipidir. Ham kapsam `/home/hakan/github/sqliters/target/perf-work/mmap-data-version-audit/pragma-gcov/sqlite3.c.gcov`.
