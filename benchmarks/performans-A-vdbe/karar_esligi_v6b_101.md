# V6b.101 — Bağımsız EXISTS ve dış covering tarama

Normal, bağımsız aynı tablo EXISTS'i dış projection'dan sütun istemediğinde C'nin dış covering tarama kararı uygulanıyor. Kapsam mevcut çözümleyici, gerçek kaynak kimliği ve tek uygun düz indeksle doğrulanıyor. EXISTS koşulu yürütme ve sütun maskesinde kalıyor. [C kaynak ve bağımsız ölçüm](/github/sqliters/target/perf-work/exists-covering-independent/sonuc.md)

270 yeni native grupta V1/V2 ayrı ayrı ham OFF 60/135'ten 96/135'e, ON 85/135'ten 109/135'e çıktı. DB SHA 135/135. Önceki 204 sayaç grubunun 72 gerçek plan iziyle birlikte ölçümü, iki normal self REPLACE SELECT akışında son rowid ve fiziksel DB'yi C ile eşitledi. Birleşik optimize DML OFF 389/400'den 390/400'e çıktı, 6017 eş. ON 400/400 ve açık ham gözlemler aynı. [Bağımsız SHA](/github/sqliters/target/perf-work/exists-covering-independent/sha256.txt) · [Tam korpus](/github/sqliters/target/perf-work/exists-covering-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,56 ms taban, 376,63 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/exists-covering-disk-verify/res/gates-validation.txt).
