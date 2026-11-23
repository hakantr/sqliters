# V6b.436 — Etkin ifadede fonksiyon kayıt aritesi

Pinli C ile 132 akış ve 972 API kaydı eş. Mevcut doğru ariteyi değiştirme etkin ifadede SQLITE_BUSY verir. Uyumlu uygulama overloadı, yerleşik tanımdan önce seçilir. Hazır function_list, boşta yapılan gerçek yerleşik tanım değişikliğinde yeniden hazırlanır. Önceki 72 fark kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v436-scalar-registration-identity-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1315 geçti, 1 başarısız, 325 yok sayıldı. ON 1313 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,70 ms taban, 379,20 ms aday. Süre oranı 1,274 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/exact-function-arity-registration-disk-verify/res/gates-validation.txt).
