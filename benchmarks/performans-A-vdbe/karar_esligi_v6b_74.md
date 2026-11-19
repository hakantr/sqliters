# V6b.74 — E28/F15 Pager ERROR sonrası test sözleşmesi

V6b.41, faz iki sonlandırma hatasının C SQLite 3.54.1 gibi pager'da saklanmasını uyguladı. Eski F15 testi aynı pager işlemi içinde daha sonraki COMMIT ve rollback çağrılarının başarılı olmasını bekliyordu. İlk hata, saklı hata ve kurtarma sonrasındaki veri ayrı doğrulanıyor.

| Eski fixture | İlk genişletilmiş kod | Yazılabilir kurtarma sonrası |
|---|---:|---|
| TRUNCATE faz iki | 1546 | Önceki veri |
| PERSIST başlık yazımı | 778 | Önceki veri |
| PERSIST sıfırlanmış başlık sonrası sync | 1034 | Yeni veri |
| Olağan DELETE silme öncesi | 2570 | Önceki veri |

İki önceki grup toplam sekiz aynı fixture'ı kullanıyor. İlk grup rollback, ikinci grup saklı hata etkinken COMMIT tekrarını ve ardından rollback'i denetliyor. PERSIST sıfır başlığı ve temiz salt okunur kip geçişleri korunuyor. Bu Rust pager fixture'larında dış sayfa referansı yok. Yazılabilir yeniden açılış gerekirse sıcak journal kurtarmasını tamamlıyor. Sağlıklı salt okunur yeniden açılış aynı değeri okuyor. [Bağımsız test ve SHA kaydı](/github/sqliters/target/perf-work/f15-expectation-candidate/sonuc.md).

C'nin arşivlenmiş doğrudan probu faz iki ve rollback'te aynı dört kodu gördü. Prob, tutulan referanslarla faz bir tekrarında da saklı kodu ölçtü. Faz iki tekrarının saklı hata dönüşü C kaynak guard'ında görülüyor. Dört veri sonucu, C'nin SQL rollback sonrasında aynı bağlantıdan okuması. Fixture'ların salt okunur ilk açılışı ve bütün referans ömrü bu ölçüm kapsamında ayrı. [C ve Rust kaynak kararları](/github/sqliters/target/perf-work/f15-expectation-audit/sonuc.md).

Aynı C ERROR arşivi F14 için de kullanıldı. Önceki `write-persistent / reuse` C/OFF/ON kaydı genişletilmiş 778, autocommit 1, ilk sabit sorgu ve ardından doğrulanmış eski tablo verisini aynı şekilde gösterdi. F14 mevcut iki G/Ç hatası sonrasında autocommit'i, aynı bağlantının tekrar okunmasını ve iki önceki satırın korunmasını denetliyor. Yeni hata koşusu eklenmedi. [F14 Rust OFF](/github/sqliters/target/perf-work/f14-recovery-expectation/res/off.txt) · [F14 Rust ON](/github/sqliters/target/perf-work/f14-recovery-expectation/res/on.txt) · [Eski C vakası](/github/sqliters/target/perf-work/v6b44/res/matrix.json).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1181 geçti, 1 başarısız, 337 yok sayıldı. ON 1169 geçti, 13 başarısız, 337 yok sayıldı. F14 ve F15 iki kipte başarılı. |
| DML | OFF 380/400, ON 400/400. Ham özetler V6b.73 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet V6b.73 ile aynı. |
| Performans tabanı | 298,12 ms taban, 375,77 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/pager-error-expectations-disk-verify/res/gates-validation.txt).
