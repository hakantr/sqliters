# V6b.325 — UNION ALL ve dolan LIMIT

Pozitif LIMIT ve OFFSET için yeterli satır oluştuğunda olağan UNION ALL kolları için gereksiz satır değerlendirmesi yapılmaz. 50 fark kapandı. 18 C salt okunur açılış ve beş odaklı Rust kontrolü doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v325-union-all-unused-arm-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1201 geçti, 1 başarısız, 329 yok sayıldı. ON 1198 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 343,74 ms taban, 435,66 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/union-all-unused-arm-disk-verify/res/gates-validation.txt).
