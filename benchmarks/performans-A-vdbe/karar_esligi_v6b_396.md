# V6b.396 — Üç parçalı RETURNING joker tanısı

144 Rust native sonucu içinde 20 wildcard tanısı C ile aynı hata kodu, mesaj ve byte ofsetine geldi. Ek 42 vaka iki kipte eş. Mevcut Group64 testi C referansıyla iki kipte geçti, tüm dosya SHA ve yeniden açılışlar eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v396-returning-three-part-grammar-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1276 geçti, 1 başarısız, 325 yok sayıldı. ON 1274 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 307,01 ms taban, 385,68 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/returning-three-part-grammar-disk-verify/res/gates-validation.txt).
