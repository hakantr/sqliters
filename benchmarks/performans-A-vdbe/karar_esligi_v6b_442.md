# V6b.442 — Varsayılan function_list sırası ve sütun metadatası

36 yerel C karşılaştırması eş. Önceki 24 fark kapandı. 3.198 satır, 204 kolon kaydı, 108 yaşam döngüsü ve 288 önceki kontrol doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v442-function-list-default-order-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1321 geçti, 1 başarısız, 325 yok sayıldı. ON 1319 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,30 ms taban, 381,60 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/function-list-default-order-disk-verify/res/gates-validation.txt).
