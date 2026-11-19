# V6b.98 — Yerleşik MIN/MAX ve veritabanı kodlaması

Yerleşik MIN/MAX biriktiricisinin iki varsayılan karşılaştırma girişi, mevcut encoding-aware değer karşılaştırmasını veritabanı kodlamasıyla çağırıyor. Açık ve özel kolasyon karşılaştırıcıları kendi çağrı yolunu kullanıyor. [C karşılaştırma ve kaynak](/github/sqliters/target/perf-work/minmax-encoding-independent/sonuc.md)

174 taban ve 174 aday native grupta V1/V2 ve OFF/ON her grupta ham eşlik 77/87'den 87/87'ye çıktı. C salt okunur açılış, ana DB baytları, UTF-8, NOCASE/RTRIM, özel kolasyon, sayısal, BLOB ve NULL korumaları eş. [Bağımsız SHA](/github/sqliters/target/perf-work/minmax-encoding-independent/sha256.txt)

Birleşik optimize adayda OFF 388/400 ve ON 400/400. Ham DML özetleri V6b.97 ile aynı. [Tam korpus](/github/sqliters/target/perf-work/minmax-encoding-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 388/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 297,74 ms taban, 373,13 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/minmax-encoding-disk-verify/res/gates-validation.txt).
