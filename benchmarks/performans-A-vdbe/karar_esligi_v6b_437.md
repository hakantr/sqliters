# V6b.437 — Hazırlanmış scalar seçimi ve ilk adım/reset

Pinli C ile 48 akış ve 852 API kaydında hazırlanmış view/trigger, tablo biçiminde function_list, reset ve reprepare davranışı sınandı. Taze AST view callback zamanlamasındaki 20 önceki kayıt farkı kaydedildi. Diğer ölçümler eş. 12 arite geri yükleme ve 180 değişmeyen kapsam dışı kontrol doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v437-prepared-scalar-generation-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1316 geçti, 1 başarısız, 325 yok sayıldı. ON 1314 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,99 ms taban, 379,65 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-scalar-generation-disk-verify/res/gates-validation.txt).
