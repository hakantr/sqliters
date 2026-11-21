# V6b.343 — reverse_unordered_selects ile yıldızlı SELECT

Yıldız ve nitelikli yıldız ifadeleri olağan tek tablo ve CROSS JOIN okumalarında C ters sırasını koruyor. 70 fark kapandı. 18 C yeniden açılışı ve altı odaklı kontrol doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v343-reverse-unordered-star-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1219 geçti, 1 başarısız, 329 yok sayıldı. ON 1216 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 385,58 ms taban, 460,67 ms aday. Süre oranı 1,195 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/reverse-unordered-star-disk-verify/res/gates-validation.txt).
