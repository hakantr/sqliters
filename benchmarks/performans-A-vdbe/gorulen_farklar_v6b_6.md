# V6b.6 — auto_vacuum farkları

D92 (düzeltildi): auto_vacuum C üreticisine taşındı. Hazırlama Btree bayraklarını ve bağlantı genelindeki nextAutovac isteğini değiştirir; normal sorgu o andaki değere dondurulur. Yazmalı programın meta7 operandı da hazırlanırken dondurulur. TEMP gerçek modunu kullanır; metin/sayı dönüşümü C Atoi ve tam sözcük eşleşmesidir. Reddedilen yetenek değişikliği de global VACUUM isteğini günceller.

D93 (düzeltildi): canlı Btree bayrağı COMMIT/ROLLBACK/savepoint sırasında disk başlığından otomatik türetilmez. Örneğin Full dosyada BEGIN IMMEDIATE içinde Incremental setter yalnız hazırlanırsa COMMIT sonrası PRAGMA 2, sonraki gerçek tablo okumasından sonra 1 verir. C bu iki aşamayı ayrı tutar; Rust da artık aynı sırayı izler.

D94 (düzeltildi): boş savepoint geri alması C BtreeSavepoint gibi sayfa1’i yeniden kurar; genel pager geri almasıyla yetinmez. Eski hazırlanmış auto-vacuum yazması daha sonra None hazırlamasıyla etkisizleşse bile Transaction adımı boş sayfa1 yaratır. Erken Halt yalnız meta7 yazımını atlar; yazma işleminin tamamını atlamaz. Step, sonraki global isteği eski değerine geri çekmez. Boş program dahil reset C gibi yeniden hazırlar.

220 API akışında stdout ve bütün .db baytları aynı; sekiz yeni test; 896 gerçek GÇ senaryosu aynı. Kanıt [karar_esligi_v6b_6.md](karar_esligi_v6b_6.md), alınmayan dallar ayrı dosyada. Bilinen açık-anahtar 15 test, DML hata kümeleri ve g06 stderr farkı sürüyor. Genel pager hata/OOM, authorizer/VFS override ve diğer API yolları tam kanıtlanmış sayılmaz. V6b ve M4 açık; sırada cache_spill, busy_timeout ve wal_autocheckpoint var. Hız ayarı yok; ileriPlanlama/ değerlendirilmedi.
