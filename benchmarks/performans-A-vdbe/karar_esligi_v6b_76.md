# V6b.76 — NOT MATERIALIZED CTE ve aynı FROM kaynağı

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

Aynı FROM listesindeki iki doğrudan alias, kabul edilen FROM'suz SELECT veya UNION ALL CTE gövdesine ve aynı lexical tanıma bağlanıyorsa C iki alias için bir sonuç hesaplıyor. Rust bu sonucu aynı FROM değerlendirmesinde paylaşıyor. Kaynak tanımı korelasyona giriyorsa, fiziksel FROM kaynağı içeriyorsa veya farklı lexical scope'taysa önceki yol devam ediyor. Şema metadatası değerlendirilirken bir CTE callback'i çalıştırılmıyor. [C kararları](/github/sqliters/target/perf-work/cte-tick-proof/sonuc.md) · [Birleşik aday](/github/sqliters/target/perf-work/cte-selfjoin-disk/sonuc.md).

93 disk grubu, iki tekrar ve 930 DB, üç DB kodlaması, beş join türü, durumlu callback ve lexical gölgeleme içeriyor. Her dosya C ile salt okunur yeniden açıldı ve integrity kontrolü başarılı. Eski C eşleşmelerinde gerileme yok. İlk dar adayda 75 join vakası C ile tamamen eşleşiyor. INSERT SELECT ve tek hedefli scalar UPDATE iki kipte aynı satırları ve fiziksel SHA değerlerini veriyor. Toplam ham eşlik OFF 87/93, ON 89/93. Yeniden açılış satırı ve fiziksel DB eşliği OFF 90/93, ON 91/93. [Ham disk matrisi](/github/sqliters/target/perf-work/cte-selfjoin-disk/res/qualified/result.json).

Mevcut `with_recursive_materialized_soft_keyword_priority_queue_and_interrupt` entegrasyon testi iki kipte geçti. Sonraki kesinti ve CTE bölümleri aynı testte çalıştı. [Odak OFF](/github/sqliters/target/perf-work/cte-selfjoin-disk/res/focus-off.txt) · [Odak ON](/github/sqliters/target/perf-work/cte-selfjoin-disk/res/focus-on.txt) · [Kanıt manifesti](/github/sqliters/target/perf-work/cte-selfjoin-disk/res/proof.sha256).

Çok hedefli UPDATE için dış scalar sorgu sonucu statement boyunca bir kez hesaplanıyor. C'nin dış OP_Once işlevine denk gelen mevcut tracker, her UPDATE bittiğinde bu statement'ın kaydını bırakıyor. CTE gövdesinin dış satıra bağlı olup olmadığı lexical tanımdan denetleniyor. Aynı adlı fiziksel tablonun sütunu bu kanıtın yerine geçmiyor.

Birleşik adayın 153 disk grubunda 1.530 DB'nin tümü C ile yeniden açıldı ve integrity sonucu temiz. İki ya da üç UPDATE hedefi, aynı bağlantıda iki ayrı UPDATE, yerel scalar CTE gövdesi, gölgeleme ve dış UPDATE altında çalışan trigger denetimleri geçildi. İlk 93 vakanın yeniden açılış satırları ve fiziksel DB SHA değerleri iki kipte 93/93 eş. Ham olay eşliği OFF 90/93 ve ON 91/93. Yeni 60 kapsam ve kontrol vakasında DB eşliği OFF 36/60 ve ON 44/60, ham olay eşliği OFF 30/60 ve ON 40/60. Önceki eş sonuçlarda gerileme yok. Kesin birleşik kaynakla odak iki kipte geçti. [Birleşik karar](/github/sqliters/target/perf-work/cte-update-once/sonuc.md) · [Ham 60 vaka](/github/sqliters/target/perf-work/cte-update-once/res/lexical_final/result.json) · [Kaynak SHA](/github/sqliters/target/perf-work/v76-reviewed/res/source.sha256).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1181 geçti, 1 başarısız, 337 yok sayıldı. ON 1171 geçti, 11 başarısız, 337 yok sayıldı. CTE tam entegrasyon testi iki kipte başarılı. |
| DML | OFF 380/400, ON 400/400. Ham özetler V6b.75 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özetler V6b.75 ile aynı. |
| Performans tabanı | 299,11 ms taban, 371,60 ms aday. Süre oranı 1,242 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/cte-once-disk-verify/res/gates-validation.txt).
