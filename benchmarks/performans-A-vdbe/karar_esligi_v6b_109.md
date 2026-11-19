# V6b.109 — İlk ATTACH ve kayıtlı ana kodlama

ATTACH hazırlığı C gibi mevcut şema yükleme yolunu çağırıyor. İlk adımda ana dosyanın gerçek kodlaması hazır ve eş kodlamalı bağlı dosyanın açılışı doğru. Açık bağlantı yaşamı ve mevcut şema cookie denetimi ATTACH'ın gerçek EXPLAIN akışıyla doğrulandı. [Kaynak ve bağımsız doğrulama](/github/sqliters/target/perf-work/attach-first-encoding-independent/sonuc.md)

CLI 32/48'den 48/48'e, native V1/V2 96/96 ve EXPLAIN 12/12. Üç kodlama, salt okunur ve yazılabilir bağlantı, normal ilk işlem ve önceden yüklenmiş şema ölçüldü. 36 `table_xinfo` kontrolü ve 18/18 fiziksel dosya C ile eş. Birleşik optimize DML OFF 390/400 ve ON 400/400, önceki ham özetle birebir. [Odak manifesti](/github/sqliters/target/perf-work/attach-first-encoding-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/attach-first-encoding-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 297,95 ms taban, 372,98 ms aday. Süre oranı 1,252 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/attach-first-encoding-disk-verify/res/gates-validation.txt).
