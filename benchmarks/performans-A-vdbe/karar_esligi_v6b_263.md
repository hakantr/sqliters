# V6b.263 — Typed Julian çoklu süre değiştiricileri

Yalnız süre ve hassasiyet bayrakları içeren typed INTEGER/REAL zincirleri, bir ham Julian dönüşümü, C birim sınırı, her adımın signed yuvarlaması ve tek final aralık kontrolünü kullanır. 1.573/1.573 C eş ve 1.122 fark kapandı. Üç kalıcı kodlama profili ile 120 C salt okunur açılışı geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v263-typed-julian-multiple-elapsed-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 308,81 ms taban, 390,83 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/typed-julian-multiple-elapsed-disk-verify/res/gates-validation.txt).
