# V6b.31 — görülen farklar

D139 kalan sekiz UTF-16 NOCASE akışında, D140 NUL sıralama/eşitlik/DISTINCT/UNIQUE ve hızlı GROUP BY örneklerinde, D141 ham Unicode aralık örneklerinde düzeltildi. Bağlı LIKE 48/48; ek sınır/tür 24/24; NUL 12/12; geniş SQL 72/72; tam program 528/528 aynı. UTF-16 CAST/metinden sayıya dönüşüm kusurları da giderildi. Ham hücre 489145 ve sayısal hücre 1587945 gözleminde fark yok.

Testler kapalı (1129, 1, 337), açık (1115, 15, 337); başarısız isimler değişmedi. DML 378/400 ve 398/400; finder stdout245/stderr244. Hız toplam 1.217x, W09 5.052x, W14 1.370x; 14 C digest farkı sıfır. Performans ayarı yapılmadı.

Genel geçersiz TEXT API temsili, NOMEM/tahsis izi, GLOB/reprepare geneli, D52/D103/E28 ve kalan V6b/M4 açıktır. Ölçülen örneklerin eşitliği tam C eşliği/yayın garantisi değildir. [Karar ve kanıt](karar_esligi_v6b_31.md).
