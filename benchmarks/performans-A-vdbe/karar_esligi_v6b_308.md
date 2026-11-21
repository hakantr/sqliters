# V6b.308 — Bileşik CAST görünümü tür bildirimi

Görünüm PRAGMA sütun meta verisinde sayısal baş CAST için C’nin UNION ALL ve çok satırlı VALUES tür kararı taşınır. 336 metadata farkı kapandı, 288 satır ve typeof sonucu korundu. 30 C salt okunur açılışı ve 6 kodlama/günlük imaj grubu eş. Tcl 128/4’ten 130/2’ye geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v302-cast-view-declared-type-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,31 ms taban, 381,34 ms aday. Süre oranı 1,261 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/cast-view-declared-type-disk-verify/res/gates-validation.txt).
