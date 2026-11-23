# V6b.449 — Kayıtlı görünüm MATCH güvenilir şema kararı

Güvenilmeyen şemadaki kayıtlı görünüm MATCH ifadesi C gibi reddedildi. Doğrudan okuma, güvenilir görünüm, VACUUM sonrası yeniden açılış ve bütünlük kontrolleri korundu. FTS dosyasındaki 14 test iki kipte geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v449-trusted-match-defense-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v449-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1328 geçti, 1 başarısız, 325 yok sayıldı. ON 1327 geçti, 2 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,25 ms taban, 384,86 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/stored-view-match-authorizer-disk-verify/res/gates-validation.txt).
