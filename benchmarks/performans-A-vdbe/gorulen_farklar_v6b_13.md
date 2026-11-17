# V6b.13 — şema gereksinimi ve açık UTF-16 sınırı

D112/D114’ün sınanan UTF-8 akışlarında gereksiz şema yükleme, yeniden hazırlama ve Btree kiralaması düzeltildi. Kodlamanın soğuk/yüklenmiş durumları ayrı tutulur; VALUES coroutine şema okuması eklendi. C’nin elediği tabloyu eski doğrulayıcı reddederse yalnız maskesi 0 ve mevcut makine korumalarından geçen C programı kullanılır.

D115 yeni, AÇIK: UTF-16le/be dosyasında bozuk şema RESET sonrası SELECT 17 WHERE 0 AND EXISTS(SELECT * FROM t), C’de satırsız tamamlanır; Rust no such table: t verir. Makinenin mevcut UTF-16 koruması bu dar geçişi engeller. 12 bağımsız codec/ifade/anahtar yoklamasında dört farklı çıktı; bütün dosya baytları aynı. Sıradaki doğruluk adımı D115.

Ana derlem: 11.534 tam program, 13.957 EXPLAIN/EQP ve 9.806 hazırlama akışı aynı. API 2.913/2.914 bütün çıktı aynı; D103 açık. Bütün 2.914 akışın bütün .db baytları aynı. Diğer bağlantı EXCLUSIVE kilit tutan 6 akış/126 sorgu da C ile aynı.

Ek üretici yoklaması ölçümde toplam süreyi 1,062x, W14’ü 1,369x yaptı; bütün sonuç özetleri aynı. Hız ayarı uygulanmadı. Mevcut test/DML/ayrışma farkları, E28, kalan PRAGMA ve M4 işleri açık; tam C garantisi verilmez. Kanıt ve dört kapı [karar_esligi_v6b_13.md](karar_esligi_v6b_13.md) içinde.
