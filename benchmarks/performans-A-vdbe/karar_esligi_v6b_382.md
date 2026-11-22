# V6b.382 — UTF-16 BINARY fiziksel indeks araması

Ana veritabanında BINARY anahtarlı CASE okumalarında karma kodlamalı arama anahtarları C ile eşleşiyor. 756 fiziksel gözlemde 240 fark kapandı. 360 karma anahtar karşılaştırması, 60 olağan yazma ve 24 bayt eş C dosya imajı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v382-physical-binary-probe-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1260 geçti, 1 başarısız, 325 yok sayıldı. ON 1257 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 296,57 ms taban, 376,39 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-binary-physical-index-disk-verify/res/gates-validation.txt).
