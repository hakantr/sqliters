# V6b.376 — Kalıcı görünümde satır CASE

Kalıcı görünümden gelen alt sorgulu satır CASE adım davranışı iki Rust kipinde C ile eşleşiyor. Exact 600 gözlemde 110 fark kapandı. 12 C yeniden açılışı ve görünüm, derinlik, yetkilendirme ve kolasyon korumaları doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v376-persisted-view-case-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1254 geçti, 1 başarısız, 325 yok sayıldı. ON 1251 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,77 ms taban, 382,62 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/persisted-view-row-case-disk-verify/res/gates-validation.txt).
