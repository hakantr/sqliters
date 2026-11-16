# V6b.12 — şema yükleme sırası

D113 düzeltildi: C prepare.c:468–477 ile aynı main → sondan başa bağlı veritabanları → TEMP sırası hem yeni yüklemede hem açık işlemdeki yenilemede kullanılır. Main, eski/yeni bağlı veritabanları ve TEMP bozukluklarının 16 birleşimi, iki bağlanma sırası, SELECT/table_info ve dört işlem durumunda ilk hata C ile eşleşir. İsim çözümleme TEMP → main → ilk bağlanan önceliğini korur.

İlk adayın açık işlem yolunu atladığı bağımsız yoklamada görüldü; ikinci yordam da düzeltildi, dört kapı son kaynak üzerinde tekrarlandı. Tam program karşılaştırıcısındaki ortak bağlı dosya hatası ayrı çalışma dizinleriyle düzeltildi; bu eski 60 kayıt ürün kusuru sayılmadı. Ham eski sonuçlar yerel kanıtta korundu.

11.240 tam program, 13.621 EXPLAIN/EQP ve 9.470 hazırlama akışı aynı. API 2.235/2.236 bütün çıktı aynı; tek açık fark D103. Bütün 2.236 API akışında bütün .db dosyaları C ile bayt bayt aynı.

D112 FROM’suz SELECT gereksiz şema okuması ve D103 eski yürütücü farkı açıktır. Yeni bağımsız iki bağlantı yoklamasında D114 görüldü: kullanılmayan CTE veya false AND alt sorgusundaki tablo için Rust gereksiz okuma kilidiyle BUSY verir; C şeması önceden yüklenmiş bağlantıda sonucu kilitsiz üretir. İki anahtar durumunda da açıktır. Mevcut 15 makine test kusuru, DML/ayrışma farkları ve E28 sürer; tam C garantisi verilmez. Kanıt, dört kapı ve alınmayan dallar [karar_esligi_v6b_12.md](karar_esligi_v6b_12.md) içindedir.
