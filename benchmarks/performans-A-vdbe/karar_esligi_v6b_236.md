# V6b.236 — SELECT üç parçalı yıldız tanısı

SELECT projeksiyonunda şema ve tablo veya alias ile nitelenmiş yıldız C gibi prepare sırasında rc ve extended rc 1, aynı mesaj ve yıldızın bayt ofsetini döndürür. 64 OFF farkı kapandı. 136 negatif ve 112 geçerli C–Rust karşılaştırması, 372 C açılışı ve 56 gerçek ON izi doğrulandı. İki mevcut runtime testinin beklentisi C tanısıyla hizalandı ve bunların diğer kontrolleri korundu. İki runtime ve yapısal AST testi iki kipte 6/6 geçti. 4.722 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v234-f8-select-projection-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,59 ms taban, 377,88 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/f8-select-projection-disk-verify/res/gates-validation.txt).
