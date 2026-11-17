# V6b.26 — görülen farklar

D131: EXPLAIN yazıcısı etkin yazıcı sayılmıyor, EOF anında erken kapanıyor ve changes eski değeri koruyordu. Bekleyen DML erken commit/FK hatası üretiyordu. İlk 72 ve raw 408 ölçümünde 396 fark düzeltildi; son 72+408 aynı; kilit/hook kapanışında 120/192 tam aynı, kalan 72 D132 hata metni farkı. Eski altı D131 akışı da aynı. Gözlenen bütün ana .db dosyaları eşit.

D132: 531 commit-hook reddinde C constraint failed, Rust commit hook vetoed commit verir. EXPLAIN içinde 72 ve bağımsız 36 doğrudan işlemin 12’sinde aynı eski hata metni farkı kaydedildi; veri/sayaç/dosya eşit. Sıradaki düzeltmedir.

20 D52 sayaç farkı 1644 akışlık eski defer matrisinde sürer; 1624 akış tam eşit ve bütün dosyalar aynı. Manual API’de tek D103 legacy çıktı farkı sürer. Bunlar normalize edilmedi veya başarı sayılmadı. NOMEM, genel interrupt/callback, readOnly Halt BUSY ve E28 gibi kanıtlanmayan yollar açık. 665 alınmayan dal ayrı listelenir.

Dört kapı: kapalı (1119, 0, 337), açık (1104, 15, 337); DML 378/400 ve 398/400; ayrışma bulucu 245 stdout / 244 stderr aynı; hız toplam 1.060x, W14 1.369x, 14 digest farkı sıfır. Kanıt [karar_esligi_v6b_26.md](karar_esligi_v6b_26.md).
