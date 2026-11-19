# V6b.72 — Kesinti C dal kapsamı

| C koşulu | Alınan çıktılar |
|---|---|
| `tokenize.c:652–669`, özel token öncesi kesinti | Lexical profilde 16.566 özel ve 39.936 olağan token. Özel token 144 kesinti ve 16.422 devam. |
| `prepare.c:788–842`, parser rc ve hata aktarımı | 3.450 doğrudan NUL-sonlu parser çağrısı. 366 hata aktarımı ve 3.084 başarılı ErrorClear. |
| `vdbeapi.c:855–867`, READY / idle temizleme | 1.932 idle temizleme ve 276 etkin peer varken temizlememe. |
| `vdbe.c:959,1149,1162,9571`, yürütme kesintisi | 276 giriş iptali ve 3.588 girişte devam. Eski bağımsız progress profili 12 callback kesintisi kaydetti. |
| `vtab.c`, eponymous cache ve constructor | 18 çağrı, 6 cache hit ve 12 gerçek constructor. Declaration 6 başarı ve 6 kesinti. Hata aktarımında 4 boş ve 2 dolu pzErr. |

| Alınmayan ilgili çıkış | Sınır |
|---|---|
| `tokenize.c`, tahsis ve SQL uzunluğu | Olağan yerel SQL gruplarında tahsis ve uzunluk hatası uygulanmadı. WINDOW/OVER/FILTER ve özel sayısal token uçları ayrı kapsam. |
| `prepare.c`, nBytes kopyası, OOM ve schemas | Sürücü NUL-sonlu SQL kullandı. Mesajsız prepare hatası ve schema retry ayrı. |
| `vdbeapi.c`, expired/reprepare | READY ve etkin ifade akışı ölçüldü. Expired VM tekrar hazırlaması ayrı. |
| `vdbe.c`, eşzamanlı yürütme | Sıralı native ölçümle loop sırasında başka thread'den kesinti, yazıcı rollback'i ve active expiration ölçülmedi. |
| `vtab.c`, declaration öneki ve constructor hataları | Yanlış önek, yeniden declaration, misuse, OOM ve recursive constructor alınmadı. |

Yeni iki C gcov profilinde hedef fonksiyonların sayaçları önceki profille birebir. Amalgamın ilgisiz sayaçlarında koşu farkı var. Eski ortak gcov dosyasının SHA-256 değeri sabit. [Lexical gcov](/github/sqliters/target/perf-work/interrupt-resume-independent/res/replayed-lexical/gcov/gcov.txt) · [Vtab gcov](/github/sqliters/target/perf-work/interrupt-resume-independent/res/replayed-warm-prior/gcov/gcov.txt) · [Önceki kapsam ayrıntısı](/github/sqliters/target/perf-work/interrupt-lexical-proof/res/sonuc.md).
