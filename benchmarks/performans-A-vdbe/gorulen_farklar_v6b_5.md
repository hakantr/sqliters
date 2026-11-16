# V6b.5 — page_size farkları

D89 (düzeltildi): page_size C gibi hazırlamada değişir, sorgu sonucu hazırlanmış programa gömülür. Step eski ayarı tekrar uygulamaz. Boş veritabanında açık işlem engeli yoktur. Geçersiz ve taşan yeni istekler, önceki VACUUM isteğini iptal eder.

D90 (düzeltildi): nextPagesize bağlantı genelindedir. Bir şemanın isteği başka şemanın VACUUM işlemini etkiler. Normal VACUUM ve INTO isteği korur; WAL kaynağına yerinde VACUUM global isteği sıfırlar. TEMP açılışı o andaki global istekle, ATTACH açılışı varsayılan boyutla gerçekleşir.

D91 (düzeltildi): Btree açılışının ham başlık boyutu/fixed/reserve bilgisi artık C'nin açılış aşamasında alınır. page_size şema okumadan, imza doğrulamadan ve SHARED kilidi almadan hazırlanır. Geçersiz imzalı 12 dosya ve iki bağlantılı EXCLUSIVE kilit denemesi C ile eşleşti. Reserve64 dosyasında 512 isteği VACUUM sonucunu C gibi 1024 yapar.

Kanıt [karar_esligi_v6b_5.md](karar_esligi_v6b_5.md), tek tek alınmayan dallar ayrı dosyada. 176 API akışında stdout ve bütün .db baytları aynı; 896 gerçek GÇ senaryosu yeniden aynı. Dört yeni regresyon testi var. Bilinen açık-anahtar 15 test, DML hata kümeleri ve g06 stderr farkı sürüyor. Genel pager hata/OOM ve henüz taşınmayan API yolları kapandı sayılmaz. V6b ve M4 açık; sırada auto_vacuum var. Hız ayarı yok; ileriPlanlama/ değerlendirilmedi.
