# V6b.265 — Typed Julian tek başlangıç değiştiricisi

Doğrudan INTEGER ve REAL Julian girdilerinde tek exact start of day/month/year takvim okumasından önce C yuvarlaması ve aralık kontrolü kullanır. Ana 225 hedefin ve ek 408 sınır hedefinin tamamı iki kipte C ile eş. İki matriste 129 ve 208 fark kapandı, 45 C ve 90 Rust salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v265-typed-julian-single-start-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,04 ms taban, 378,40 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/typed-julian-single-start-disk-verify/res/gates-validation.txt).
