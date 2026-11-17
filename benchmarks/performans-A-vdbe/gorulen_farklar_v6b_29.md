# V6b.29 — görülen farklar

case_sensitive_like hazırlama etkisinin eski 96/288 farkı giderildi. BUSY altında işlev/builtin bayrakları, scalar/aggregate kayıt, ad/arity sırası ve UTF-16 Column/Concat/işlev sonucu C ile hizalandı. Yeni 1116 katı native akışın bütün gözlemleri ve dosya baytları eşit. 83232 desen sorgusu bu akışların içindedir; ayrı bir 83232 bağlantı sayılmaz.

**D139 açık:** 48 parametreli LIKE akışında sqlite3_stmt_status farklı. Veri/hata/diğer sayaçlar ve dosyalar aynı; bu 48 örnek geçiyor sayılmadı. Bağlı desenle yeniden üretim sıradaki doğruluk işidir.

**F4:** kapalı koşumda LIKE makineye geçtiğinden bilinen sticky-interrupt beklentisi de görünür oldu. 36 ayrı C API akışı makine davranışını doğruladı. Test değiştirilmedi. Çalışma alanı kapalı (1121, 1, 337); açık (1107, 15, 337); açıkta önceki 15 başarısızlık aynı. DML 378/400 ve 398/400; finder stdout245/stderr244; hız toplam 1.207x, W14 1.368x, 14 digest farkı sıfır.

D52/D103/E28 ve kalan PRAGMA/M4 açık. Genel function API/NOMEM/UTF-16 NOCASE anahtarları/GLOB tamamlandı sayılmaz. 729 alınmayan dal ayrı sınıflandırıldı. Kanıt [karar_esligi_v6b_29.md](karar_esligi_v6b_29.md).
