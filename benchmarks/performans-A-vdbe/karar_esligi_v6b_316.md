# V6b.316 — date5, func7 ve func8 test paketleri

Mevcut Tcl paketleri iki kipte 874/874, 73/73 ve 8/8 ile normal koşuya alındı. Takvimde kip başına 2.622 ayrı C sonucu, matematikte 73 ham C bölüm sonucu ve 7 C kullanıcı işlevi sonucu eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v312-enable-date5-func8-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1192 geçti, 1 başarısız, 331 yok sayıldı. ON 1189 geçti, 4 başarısız, 331 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,63 ms taban, 378,94 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/enable-date5-func7-func8-workspace-disk-verify/res/gates-validation.txt).
