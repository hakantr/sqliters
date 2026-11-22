# V6b.386 — UTF-16 BINARY CASE üyeliği

Salt okunur CASE alt sorgusunda BINARY IN ve NOT IN anahtarları aynı kodlamayla karşılaştırılıyor. 558 gözlemde 32 fark kapandı. Altı C yeniden açılışı ve NULL, boş sonuç, satır üyeliği ve yeniden hazırlama kontrolleri geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v386-row-case-binary-in-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1264 geçti, 1 başarısız, 325 yok sayıldı. ON 1261 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,02 ms taban, 383,50 ms aday. Süre oranı 1,261 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-binary-in-disk-verify/res/gates-validation.txt).
