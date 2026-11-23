# V6b.463 — pragma_function_list sütun kullanılmayan READ kararı

COUNT(*) ve sabit yansıtmalar C gibi boş sütun adıyla hazırlama yetkisi soruyor. Ret hiçbir satır döndürmüyor. Yoksayma, iç PRAGMA yetkilendirmesi ve adlandırılmış sütun kararları korunuyor. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v463-function-list-anonymous-authorization-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v463-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1339 geçti, 0 başarısız, 325 yok sayıldı. ON 1339 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,40 ms taban, 382,18 ms aday. Süre oranı 1,285 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/function-list-anonymous-read-authorization-disk-verify/res/gates-validation.txt).
