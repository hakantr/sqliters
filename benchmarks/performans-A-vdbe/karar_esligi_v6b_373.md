# V6b.373 — Alt sorgulu satır CASE adım davranışı

İki Rust kipinde salt okunur satır CASE alt sorguları gerçek adım sınırlarında yürütülüyor. Altı disk profilinde 144 ifade sonucu C ile karşılaştırıldı. Önceki 60 fark kapandı. Odaklı C ve Rust adım kontrolleri geçti. Özgün rowvalue8 paketi workspace koşusuna alındı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v373-row-case-read-revised-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1251 geçti, 1 başarısız, 326 yok sayıldı. ON 1248 geçti, 4 başarısız, 326 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,20 ms taban, 385,75 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-read-disk-verify/res/gates-validation.txt).
