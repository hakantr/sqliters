# V6b.10 — encoding hazırlama farkları

D108 (sınanan kapsamda düzeltildi): encoding hazırlamada değişir; BEGIN içinde de geçerlidir. Sekiz tam ad, yerel UTF16 seçimi, boşluk kırpmama ve EncodingFixed sonrası geçersiz adları bile yok sayma C ile karşılaştırıldı. Geç sentaks hatasından önceki etkiler korunur; pragma_encoding kaydı eklendi.

D109 (sınanan kapsamda düzeltildi): her geçerli atama, aynı UTF8 değeri olsa da bekleyen ifadeleri advisory expire eder. Bekleyen getter son kodlamayla yeniden hazırlanır; eski setter de gerektiğinde yeniden hazırlanıp kendi ayarını uygular. Çalışan ifade tamamlanabilir. Bu, sıradan donmuş PRAGMA getterine göre farklı C kuralıdır.

D110 (sınanan kapsamda düzeltildi): kodlama sorgusu hazırlamada gerçek şemayı okur. İlk main şema yüklemesi dosyadaki kodlamayı bağlantıya geri getirir, boş pagerlerin bekleyen kodlaması eşitlenir. Setter dosya kilidi istemez; getter EXCLUSIVE engelinde BUSY alabilir. user_version ile oluşmuş sayfa 1 yeni kodlamayla ilk tablo kurmayı engellemez. Mevcut UTF-8/UTF-16le/UTF-16be dosyalarının baytları değişmedi.

D103 önceki adımdan AÇIK. D111 AÇIK: c_generated_program, gerçek prepare girişindeki EncodingFixed güncellemesini yapmadığından 33 kabul edilebilir atamaya yanlış unsupported encoding hatası döndürüyor. Normal prepare/EXPLAIN eşleşir; bu 33 kayıt eşleşme sayılmadı ve sıradaki doğrulama düzeltmesidir. Gizli tam-program doğrulama API’sinin ilk kullanımda sanal tablo bağlamaması kanıt sınırı olarak korundu; ayrı eş bağlama hazırlığıyla 550 program ayrıca karşılaştırıldı. Bilinen test/DML/ayrışma, E28 ve genel UTF-16/geçersiz UTF-8 iş listesi açık. Ayrıntılar [karar_esligi_v6b_10.md](karar_esligi_v6b_10.md) içinde. Hız ayarı yapılmadı; ileriPlanlama değerlendirilmedi.
