# V6b.11 — doğrulama API’si ve yeni şema farkları

D111 (bu derlemde düzeltildi): önizleme gerçek prepare gibi EncodingFixed durumunu günceller; 33 yanlış encoding hatası kalmadı. SELECT sanal tablo bağlantısını hazırlar; 1.127 bağlanmamış kayıt kalmadı. İlk adayda açılan 60 şema çerezi farkı, gerçek şemayı yükleyerek kapandı. Aynı ilk-kullanım derleminde 11.172 tam program C ile aynı; ilave EXPLAIN hazırlığı kullanılmadı.

D112 (yeni, AÇIK): iki bozuk şema ve RESET sonrasında C FROM’suz SELECT 1 hazırlamasını kabul ediyor; Rust gereksiz şema okumasıyla malformed database schema (q) veriyor. S10’da da vardı; genel SELECT şema gereksinimi henüz C kararını tam yansıtmıyor.

D113 (yeni, AÇIK): aynı girdide SELECT * FROM aux.u için C önce aux.u bozukluğunu, Rust TEMP q bozukluğunu bildiriyor. C main, sondan başa bağlı veritabanları, TEMP sırasını izler; fiziksel Rust yükleyicisi şu an 0..slot_cnt ilerler. Sonraki doğruluk adımı D112/D113’tür.

D103 eski yürütücü farkı ve mevcut diğer iş listesi açıktır. Dört kapı ve her alınmayan dal [karar_esligi_v6b_11.md](karar_esligi_v6b_11.md) içinde. Hız ayarı uygulanmadı; ileriPlanlama değerlendirilmedi. Tam C garantisi verilmez.
