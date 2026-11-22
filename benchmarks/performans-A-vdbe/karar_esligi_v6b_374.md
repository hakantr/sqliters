# V6b.374 — UTF-16 alt sorgulu satır CASE sıralayıcısı

Yerleşik BINARY sıralayıcısı kullanan UTF-16 CASE alt sorguları iki Rust kipinde yürütülüyor. 12 disk profilinde 720 gözlem C ile karşılaştırıldı. Önceki 80 fark kapandı. C ve Rust adım, sıralama ve yeniden hazırlama kontrolleri geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v374-row-case-binary-sorter-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1252 geçti, 1 başarısız, 326 yok sayıldı. ON 1249 geçti, 4 başarısız, 326 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,92 ms taban, 383,01 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-binary-sorter-disk-verify/res/gates-validation.txt).
