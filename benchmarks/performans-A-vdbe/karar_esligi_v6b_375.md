# V6b.375 — Canlı iki tablo SELECT imleçleri

Aynı bağlantıda olağan UPDATE veya DELETE sonrası doğrudan iki rowid tablosunun SELECT adımları C ile eşleşiyor. Exact API gözleminde iki kipte 72/72, 48 bayt eş ana dosya grubu ve 144 başarılı C yeniden açılışı. Özgün delete2 paketi çalışma alanı koşusuna alındı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v375-live-table-join-read-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1253 geçti, 1 başarısız, 325 yok sayıldı. ON 1250 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,37 ms taban, 386,00 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/live-table-join-read-disk-verify/res/gates-validation.txt).
