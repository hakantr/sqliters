# V6b.79 — TRUE ve FALSE için C dal kapsamı

| C yolu | Yerel kanıt |
|---|---|
| `resolve.c` TK_TRUEFALSE/TK_TRUTH | C örtük ad çözümü, COLLATE ve parantez bağlamları. |
| `OP_IsTrue` | 12 dönüş, 480 değer paketi iki kipte C eş. |
| `sqlite3ExprIfTrue` ve `sqlite3ExprIfFalse` | 12 ve 81 truth dalı. Normal IS ve terslik kontrolleri. |
| UPDATE ve DELETE | 72 dosya ve 648 çapraz C/Rust yeniden açılışı. |

[Native ölçüm](/github/sqliters/target/perf-work/is-truth-native-independent/sonuc.md) · [C gcov](/github/sqliters/target/perf-work/is-truth-native-independent/res/extra-c-coverage/sqlite3.c.gcov).

Birleşik adayın ek etkileşim profilinde OP_IsTrue 15 ve generated maskesi sqlite3ExprColUsed 594 çağrı. İndeks maske yeniden hesaplaması 36, örtücü indeks yürüyüşü 9. [Etkileşim C kapsamı](/github/sqliters/target/perf-work/v79-truth-independent/sonuc.md).
