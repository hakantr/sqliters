# V6b.17 — D117, D118 ve D119

D117’nin sıcak IOERR_READ yoklamaları iki anahtarda eşitlendi. 30 hata-bölgesi akışında sonuç, hata kodu/ileti ve .db baytları aynı. 13.181 program aynı; API 4157/4158 aynı (eski D103); bütün 4.158 akışın .db baytları aynı.

Bekleyen WAL açılışını atlayan ilk guard kabul edilmedi: 224/896 WAL dosyasında +1 sayaç farkı vardı. Son guard ile 896/896 eşleşti. Geçişte C/R journal silme zamanlaması ve önceki TEMP/journal GÇ gözlemleri açık.

D119 açık: sayfa1 IOERR_READ C’de hatayla biterken Rust başarılı döndürüyor. D118 açık: aynı SHORT_READ altında C 26/26 file is not a database, Rust 10/522 disk I/O error. Sonraki iş D119 hata aktarımı ve sayfa1/kısa okuma yoludur. Genel hot-journal bayrakları/sync ve E28 hata durumu da açık.

Kanıt [karar_esligi_v6b_17.md](karar_esligi_v6b_17.md) içinde; tam C eşliği iddiası yok.
