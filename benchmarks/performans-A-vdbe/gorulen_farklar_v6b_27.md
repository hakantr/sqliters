# V6b.27 — görülen farklar

D132 kapandı: 531 hook veto hatası artık C gibi constraint failed. EXPLAIN kapanışındaki 72 ve doğru yazmalı doğrudan işlemlerdeki 36 eski hata metni farkı giderildi. Son 192/192 EXPLAIN ve 26/36 doğrudan akış tam aynı; kalan 10 D133 autocommit farkı. Bütün ana dosya baytları aynı. Önceki 72+408 D131 akışı da aynı.

D133: hook ile reddedilen transaction savepoint RELEASE, C autocommit 0 iken legacy Rust 1 veriyor. Düzeltilmiş yazmalı fixture’ların 10’unda açık; ON/UTF8 makine örnekleri aynı. Sıradaki doğruluk işidir.

D52’nin 20 legacy sayaç farkı ve D103’ün tek eski manual API farkı açık. E28 pager ERROR, kalan PRAGMA/M4 ve genel C eşliği tamamlanmadı. 666 alınmayan dal tek tek sınıflandırıldı.

Dört kapı: kapalı (1120, 0, 337), açık (1105, 15, 337); DML 378/400 ve 398/400; finder stdout 245/245, stderr 244/245; hız toplam 1.051x, W14 1.367x, 14 digest farkı sıfır. Kanıt [karar_esligi_v6b_27.md](karar_esligi_v6b_27.md).
