# V6b.14 — UTF-16 boş yol ve sayaçlar

D115 düzeltildi: C’nin yalnız kontrol akışı, tamsayı hazırlaması ve başarılı Halt ile satırsız biten maskesi 0 programı UTF-16’da da çalışır. Unicode sonuç ifadesi ve çalıştırılmayan işlevler de bu sınır içinde karşılaştırıldı. Genel UTF-16 makine koruması ve metin/satır üreten genel yollar için eşlik bu adımla tamamlanmaz.

D116 yeni ve düzeltildi: yeniden hazırlama önceki VM_STEP/RUN sayaçlarını siliyordu. C vdbeaux.c:152–153 gibi sayaçlar kopyalanır ve REPREPARE artırılır. İlk 120 yalnız-sayaç farkı kapandı; ayrı C yoklaması sıfırlama bayraklarını da doğruladı.

API 3.075/3.076 bütün çıktı aynı; tek fark önceki D103. Bütün 3.076 akışın bütün .db baytları aynı. 11.654 tam program, 18 iki-bağlantı akışı/378 sorgu ve altı sayaç akışı eşleşti. Dört kapı ve alınmayan dallar [karar_esligi_v6b_14.md](karar_esligi_v6b_14.md) içinde.

Mevcut test/DML/ayrışma farkları, E28, kalan PRAGMA, M4 ve genel UTF-16 iş listesi açık. Hız ayarı uygulanmadı; tam C garantisi verilmez.
