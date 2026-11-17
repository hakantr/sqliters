# V6b.30 — görülen farklar

D139'un ana 48 akışında 40 tam eşit, 8 UTF-16 NOCASE legacy status farkı açık. 144 bound EXPLAIN/EQP, 48 bind ömrü, 84 V1/V2-parametre ve 144 tam program akışı aynı; bütün ana dosya baytları eşit. Dummy Variable eksikliği kaynaklı VM_STEP farkı da giderildi.

D140 NUL/NOCASE (12 native akış) ve D141 UTF-16BE Unicode üst sınırı (ek matrisin 8 akışı) veri farkları önceki V6b.29 ikilisinde de aynı. Sonraki doğruluk işi kodlama/collation/ham token baytlarıdır. Ek sınır-türleri matrisindeki 24 akışın yalnız 4'ü tam aynı; kalanlar başarı sayılmaz. C'nin beklenmedik üst-sınır çıktısı da eşlik hedefidir.

Kapalı test (1123, 1, 337), açık (1109, 15, 337); başarısız isimler önceki adımla aynı. DML 378/400 ve 398/400; finder stdout245/stderr244. Hız toplam 1.227x, W09 5.009x, W14 1.372x; 14 C digest farkı sıfır. 776 alınmayan dal ayrı sınıflandırıldı. D52/D103/E28 ve kalan V6b/M4 açık. Kanıt [karar_esligi_v6b_30.md](karar_esligi_v6b_30.md).
