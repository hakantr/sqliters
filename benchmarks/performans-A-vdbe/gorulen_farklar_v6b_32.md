# V6b.32 — görülen farklar

mmap hazırlama etkisi/VFS sınırı ve TEMP’nin ilk spill’de açılması taşındı. 270 TEMP akışı, 144 tam mmap programı ve 78 ek EXPLAIN/EQP/yürütme ifadesi aynı. 250 native mmap akışında 240 aynı; 4 eski DDL offset (D144) ve 6 mapped-reader limiti (D142) açık. Bütün kalıcı dosyalar aynı. Gerçek mapped-page lease / nFetchOut / mmap-unmap yaşamı tamamlanmadı.

Testler kapalı (1128, 4, 337), açık (1114, 18, 337); önceki başarısızlara C dışı F10–F12 beklentileri eklendi. Beklentiler değiştirilmedi; ayrı C VFS gözlemcisiyle doğrulandı. Hedef C’de lock_status PRAGMA’sı yoktur; F12 için FILE_CONTROL_LOCKSTATE=NOTFOUND kanıtıdır. DML 378/400 ve 398/400, finder stdout245/stderr244 aynı. Hız son kaynakta 1.224x; önceki kaynak ölçümleri arşivde, 14 C digest farkı sıfır.

D142, D143/D57 module_list, D144, genel E27/E28 ve kalan V6b/M4 açık. [Karar ve kapsam](karar_esligi_v6b_32.md).
