# V6b.332 — MIN/MAX ile gruplama anahtarının kullanımı

MIN/MAX ile hesaplanmış GROUP BY anahtarının sonuçta kullanımı C ile eş. 20 yerel fark kapandı. On C ve Rust kontrolü, 24 olağan SQL biçimi ve 18 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v332-group-minmax-key-projection-revised-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1208 geçti, 1 başarısız, 329 yok sayıldı. ON 1205 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,60 ms taban, 381,31 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/group-minmax-key-projection-disk-verify/res/gates-validation.txt).
