# V6b.322 — Bileşik SELECT LIMIT ve OFFSET değerlendirme sırası

Olağan tablo kollarındaki UNION ALL UNION INTERSECT ve EXCEPT için 800 ifade sırası farkı kapandı. 88 sorgu biçimi, 18 C salt okunur açılış ve 24 Rust okuyucu/yazıcı çifti C ile eş. Yeni odaklı test iki kipte geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v322-compound-limit-order-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1198 geçti, 1 başarısız, 329 yok sayıldı. ON 1195 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 558,55 ms taban, 589,46 ms aday. Süre oranı 1,055 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/compound-limit-order-disk-verify/res/gates-validation.txt).
