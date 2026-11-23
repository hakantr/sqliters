# V6b.462 — pragma_function_list adlandırılmış sütun yetkilendirmesi

Adlandırılmış sütunların izin, yoksayma ve ret kararları 480 doğrudan C profili ve 4.068 kayıtta eşleşti. 192 ret satır döndürmedi. Ad yazımı, şema ve tanı doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v462-function-list-named-authorization-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v462-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1338 geçti, 0 başarısız, 325 yok sayıldı. ON 1338 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,48 ms taban, 382,34 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/function-list-named-column-authorization-disk-verify/res/gates-validation.txt).
