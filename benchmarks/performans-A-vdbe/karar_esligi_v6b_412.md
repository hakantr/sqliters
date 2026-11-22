# V6b.412 — Çok kollu birincil ve karışık OR

Birincil ve ikincil kollar C planındaki adım sınırlarıyla aynı geçici anahtar kümesini kullanıyor. Dokuz biçim 144 UTF-16 sonucu ve 216 tam EXPLAIN planını eşledi. Bağımsız birincil artı üç ikincil tanığı ek 16 sonucu ve 24 planı kapattı. Önceki 312 geniş ikincil plan korunuyor. [Bağımsız kanıt](/github/sqliters/target/perf-work/v412-without-rowid-primary-arms-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1292 geçti, 1 başarısız, 325 yok sayıldı. ON 1290 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,62 ms taban, 379,27 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-many-primary-or-disk-verify/res/gates-validation.txt).
