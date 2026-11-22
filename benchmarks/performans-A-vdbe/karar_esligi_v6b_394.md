# V6b.394 — UTF-16 sabit rowid listesi taraması

660 gözlemde tamsayı eğilimli tek alanlı sabit rowid listeleri için 48 UTF-16 sonucu C ile eşleşti. Önceki 558 envanter gözlemi ve üyelik kontrolleri aynı. C 22/22, beş odak Rust testi iki kipte ve bağlantı korumaları geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v394-row-case-literal-rowid-list-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1275 geçti, 1 başarısız, 325 yok sayıldı. ON 1272 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 310,90 ms taban, 386,20 ms aday. Süre oranı 1,242 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-literal-rowid-list-disk-verify/res/gates-validation.txt).
