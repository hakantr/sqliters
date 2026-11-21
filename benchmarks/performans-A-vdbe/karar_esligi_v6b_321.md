# V6b.321 — Olağan SELECT LIMIT ve OFFSET değerlendirme sırası

Olağan tablolardaki DISTINCT GROUP BY ve JOIN sorguları LIMIT ve OFFSET değerlerini satır hazırlamadan önce değerlendirir. 180 başlangıç farkı kapandı. 54 sorgu, 18 C salt okunur açılış ve beş odaklı Rust karşılaştırması doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v321-early-ordinary-limit-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1197 geçti, 1 başarısız, 329 yok sayıldı. ON 1194 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,54 ms taban, 381,00 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/early-ordinary-limit-disk-verify/res/gates-validation.txt).
