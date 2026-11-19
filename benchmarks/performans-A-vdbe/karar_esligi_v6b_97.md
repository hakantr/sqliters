# V6b.97 — Görünüm UPDATE ve INSTEAD OF trigger sırası

Normal görünüm UPDATE'i C'nin geçici görünüm taramasına uygun yan etkisiz aralığı taşımak için V6b.96 ile aynı kapsam denetimini kullanıyor. SET değer hazırlığı, dış filtre ve trigger eski/yeni değer akışı mevcut yürütme yollarında. [C kaynak ve bağımsız kanıt](/github/sqliters/target/perf-work/view-update-order-independent/sonuc.md)

V1 ve V2'nin her birinde ham API eşliği OFF 27/144'ten 108/144'e, ON 66/144'ten 120/144'e çıktı. Kalıcı log, ana DB SHA ve C açılışı OFF 30/144'ten 108/144'e, ON 68/144'ten 120/144'e çıktı. Ana tablo satırları 144/144. Önceki 282 DELETE grubu ham ve fiziksel olarak aynı. Birleşik optimize DML OFF 388/400, ON 400/400 ve önceki ham özetler eş. [Bağımsız SHA](/github/sqliters/target/perf-work/view-update-order-independent/sha256.txt) · [Tam korpus](/github/sqliters/target/perf-work/view-update-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 388/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,67 ms taban, 380,82 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-disk-verify/res/gates-validation.txt).
