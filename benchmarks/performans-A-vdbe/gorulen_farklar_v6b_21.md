# V6b.21 — database_list

D123 hazırlama anı/expire, D124 geçerli UTF-16 sabit metin sonuçları ve D125 aktif listeyi okuyucu sayma kusurları bu dilimde giderildi. 12 eski/yeni + 210 genişletilmiş + 12 dosyaya bağlı aux akışı sonuç, hata, sayaç ve bütün .db baytlarında C ile eşleşti.

Ölçüm aracının farklı dosya yolu, boş protokol satırı ve stdout vermeyen kapsam sürücüsü sorunları da ayrı kaydedildi; ürün kusuru olarak sayılmadı. 573 alınmayan dal, D103, bozuk UTF kodlama, E28, kalan PRAGMA/M4 ve diğer açık farklar sürüyor. Kanıt [karar_esligi_v6b_21.md](karar_esligi_v6b_21.md) içinde.
