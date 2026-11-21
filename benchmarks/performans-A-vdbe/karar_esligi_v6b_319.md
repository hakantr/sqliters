# V6b.319 — LIMIT ve OFFSET tam sayı kurallarını C ile eşle

Tek tablo ve tablosuz SELECT yürütmesinde C kayıpsız tam sayı dönüşümü, negatif LIMIT ve sıfır LIMIT sırası uygulandı. 31 geçerli sorgu, kip başına 180 hata girdisi ve 18 C salt okunur açılış eş. LIMIT Tcl paketi iki kipte 122/122 ile etkin. [Bağımsız kanıt](/github/sqliters/target/perf-work/v319-limit-must-be-int-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1195 geçti, 1 başarısız, 329 yok sayıldı. ON 1192 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,71 ms taban, 381,78 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/limit-must-be-int-disk-verify/res/gates-validation.txt).
