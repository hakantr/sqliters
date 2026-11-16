# V6b.8 — görülen farklar

D99 (taşınan kapsamda düzeltildi): locking_mode hazırlamada uygulanır; sonuç sabit metin olarak dondurulur. Şemasız sorgu bağlantı varsayılanıdır; main pager kipinden farklı olabilir. Global setter TEMP’i atlar; yerel setter varsayılanı değiştirmez; bellek/özel TEMP/immutable pagerler exclusive kalır.

D100 (taşınan kapsamda düzeltildi): pragma_locking_mode kaydı eklendi. Yalnız schema gizli sütunu olan tablonun bağlama konumu düzeltildi. C’nin azArg[0]/azArg[1] davranışı korunur: bağlanmış şema adı çıktıdaki gizli sütunda NULL’dır.

D101 (normal yaşam döngüsünde düzeltildi): changeCountDone işlemden uzun yaşayabilir. EXCLUSIVE kilit tutulurken ardışık commit temiz sayfa 1’i zorla yazmaz; playback/savepoint/unlock bayrağı C gibi sıfırlar. Sayfa 1 zaten kirliyse fiziksel yazım C gibi yeni sayaç/sürüm damgası basar. VACUUM ardından DML dosyası da eşleşir. E28 ve genel yazım hatası durumu açık kalır.

D102 (sınanan dilimde düzeltildi): PRAGMA sentaksında işaret yalnız sayı öncesinde geçerlidir; NOTHING reddedilir, WITH ve CTIME sözcükleri kabul edilir. Erken hata TEMP açmaz. Geçerli PRAGMA indirgemesinden sonraki fazla token hata verse de daha önce yapılan hazırlama etkileri, TEMP açılışı ve ayarları korunur.

D103 (AÇIK): eski yürütücü tüketilmeyen örtük TVF argümanını tekrar denetlemiyor. `SELECT locking_mode,schema FROM pragma_locking_mode('aux') WHERE schema='main'` C ve anahtar açık Rust’ta sıfır satır; kapalı Rust’ta exclusive/NULL satırı verir. Girdi matristen çıkarılmadı, başarı sayılmadı. Makineye geçişte kapatılacak genel yürütücü kusurudur.

D104 (sınanan hazırlama yollarında düzeltildi): iletisiz prepare hatası önceki sqlite3_error_offset konumunu korur; başarı konumu temizler. Üretici Parse durumu bağlantıdaki önceki konumdan başlar. İletili sentaks hatası, soğuk TEMP iletisiz hatası ve araya başarılı prepare/boşluk yürütme koyma C API’si ile karşılaştırıldı.

Program, satır/hata, dosya baytı, kilit ve GÇ kanıtı [karar_esligi_v6b_8.md](karar_esligi_v6b_8.md) içinde. D103, bilinen 15 açık-anahtar test hatası, DML/ayrışma farkları ve ayrıca kanıtlanmamış dallar açıktır. V6b tamamlanmış ve tam C garantisi verilmiş sayılmaz. Hız ayarı yok; ileriPlanlama değerlendirilmedi.
