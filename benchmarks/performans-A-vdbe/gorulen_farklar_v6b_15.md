# V6b.15 — threads

PRAGMA threads ve pragma_threads taşındı. Hazırlama etkisi, donmuş sonuç, reset/expire, bağlantı/şema kapsamı, geçersiz değer, 31 bit maske ve 8 işçi üst sınırı C ile eşleşir. Sabit üreticisinin kapalı THREADSAFE kolundan aldığı 0, başvuru derlemesinin 8’iyle düzeltildi.

13.009 tam program ve 3.995/3.996 API akışının bütün çıktısı aynı; tek fark önceki D103. Bütün 3.996 akışın bütün .db baytları aynı. threads=8 ile 40.000 satırlık uzun metin indeksleme ve integrity_check de C ile aynı. İşçi sınırının taşınması paralel sıralayıcı uygulaması değildir.

Sonraki shrink_memory yoklamasında C hazırlamada cache boşaltırken Rust adımlamayı bekliyor. Ayrı VFS kaydı mevcut başlık kontrolü okuma aralığı farkını da gösterdi; bu iki gözlem başarı sayılmadı. Genel iş listesi, E28 ve diğer C farkları açık. Kanıt [karar_esligi_v6b_15.md](karar_esligi_v6b_15.md) içinde.
