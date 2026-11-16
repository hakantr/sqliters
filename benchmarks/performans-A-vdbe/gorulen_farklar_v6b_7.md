# V6b.7 — bağlantı ve cache hazırlama farkları

D95 (düzeltildi): cache_spill, busy_timeout ve wal_autocheckpoint üreticiye taşındı. Ayarlar hazırlamada değişir; sonuç programa dondurulur. Spill eşiği yerel, etkinlik globaldir; autocommit koruması korunur. Geçersiz busy_timeout özel handlerı korur, geçersiz wal_autocheckpoint callback’i kapatır. Eski setter step sırasında sonraki ayarı geri almaz; yeniden hazırlama gerektiğinde C gibi tekrar uygular.

D96 (düzeltildi): TEMP önce derleme varsayılanı ile açılır, ardından nextPagesize uygulanır. Örneğin page_size=1024 sonrası ilk temp.data_version C gibi 2’dir; temp.page_size=512 hazırlanınca 3 olur. Pcache geometrisi boyut değişiminde hemen güncellenir.

D97 (düzeltildi): C tarafından üretilmiş EXPLAIN/EQP komut listesi doğrudan döner. EXPLAIN PRAGMA cache_spill, başka bağlantı EXCLUSIVE kilit tutarken Btree okuması/özel busy callback çağırmaz. Canlı auto_vacuum bayrağı da bir okuma başlatılarak değiştirilmez.

D98 (düzeltildi): ilk main şema okumasının kayıtlı metin kodlamasını yüklemesi bekleyen ifadeleri advisory expire eder. Çalışan spill sorgusu 483’te kalırken, henüz çalışmamış spill sorgusu yeniden hazırlanıp 17 verir. Bekleyen eski busy_timeout setter da yeniden hazırlamada kendi 17 ayarını uygular. Bu fark 8 sayfa boyutunda iki bağlantı açılışıyla karşılaştırıldı.

Program, API, dosya baytı, özel callback ve gerçek G/Ç kanıtları [karar_esligi_v6b_7.md](karar_esligi_v6b_7.md) içinde; her alınmayan dal ayrı dosyada. Bilinen 15 açık-anahtar testi, DML hata kümeleri ve genel hata yolları açık kalır. V6b ve M4 tamamlanmış sayılmaz. Hız ayarı yok; ileriPlanlama/ değerlendirilmedi.
