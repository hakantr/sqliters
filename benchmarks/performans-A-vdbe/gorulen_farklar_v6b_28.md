# V6b.28 — görülen farklar

D133 kapandı: hook ile reddedilen RELEASE, SQL savepoint’ini ve autocommit 0 değerini korur. Eski 10 örnek şimdi aynı. D134 kapandı: legacy COMMIT/transaction RELEASE callback sırasında C gibi autocommit 1 yayımlar. Yeni 144 callback, 72 kilit ve 96 FK akışı tam aynı; eski başlangıç farkları sırasıyla 60/45/80 idi. Önceki 36 doğrudan hook örneği de tamamen aynı; bütün ana dosya baytları eşit.

D52’nin 20 sayaç farkı, D103 ve E28 açık. case_sensitive_like başlangıçta 288 akışın 96’sında farklı; üretici/bağlantı işlev kaydı sıradaki iş. Genel callback/iş parçacığı, tahsis veya hata fazları tamamlandı sayılmaz. 664 alınmayan dal ayrı sınıflandırıldı.

Dört kapı: kapalı (1121, 0, 337), açık (1106, 15, 337); DML 378/400 ve 398/400; finder stdout 245/245, stderr 244/245; hız toplam 1.058x, W14 1.369x, 14 digest farkı sıfır. Kanıt [karar_esligi_v6b_28.md](karar_esligi_v6b_28.md).
