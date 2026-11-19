# V6b.76 — CTE C kararları

| C yolu | Kanıt |
|---|---|
| `flattenSubquery` koşulları 7 ve 17c | FROM'suz tek SELECT ve ölçülen FROM'suz UNION ALL birleştirilmeden kaldı. |
| `CteUse.nUse` | Aynı FROM listesindeki iki alias için C bir sonuç ve bir duplike cursor kullanıyor. İki ayrı compound kolu ve doğrudan tek kullanım kendi çağrı sayısını koruyor. |
| `isSelfJoinView` ve `fromClauseTermCanBeCoroutine` | 12 odak sorgu. C planı, paylaşılan iki alias için bir materialization gösteriyor. |
| `sqlite3CodeSubselect` dış kullanım | Çok satırlı scalar UPDATE'de ayrı OP_Once. Mevcut küçük self-join yamasının kapsamı aynı FROM içindeki paylaşım. |

[C profil ve kaynak](/github/sqliters/target/perf-work/cte-tick-proof/res/c-gcov-cte.txt) · [Scalar UPDATE C programı](/github/sqliters/target/perf-work/cte-selfjoin-disk/res/c-update-multi-explain.txt).

Son adayda top-level tablo UPDATE'i C'nin dış OP_Once decision'ına göre var olan scalar cache kaydını etkinleştiriyor. C'nin iç materialization ve dış scalar ömrü ayrı. İki ve üç hedefli, tekrar yürütülen, lexical gölgeli, hedef tabloyu yerel scalar okuyabilen ve üst statement'ın altında trigger çalıştıran vakalar ölçüldü. [Son kapsam ve C planı](/github/sqliters/target/perf-work/cte-update-once/sonuc.md).
