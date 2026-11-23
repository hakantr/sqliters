# V6b.431 — Çalışan deyim sırasında uygulama fonksiyonunu değiştirme ve kaldırma

48 etkin kayıt vakası ve 912 API kaydı C ile eşleşti. Önceki 336 kontrol korundu. Yeni sabit ve değişken arite testi OFF ve ON koşularında geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v431-active-scalar-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1308 geçti, 1 başarısız, 325 yok sayıldı. ON 1306 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,08 ms taban, 378,83 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/active-scalar-registration-disk-verify/res/gates-validation.txt).
