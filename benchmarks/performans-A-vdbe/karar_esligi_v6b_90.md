# V6b.90 — Normal SELECT sütun adları

`short_column_names` ve `full_column_names` için normal tek tablo SELECT hazırlamasında C'nin ürettiği sonuç adları korunuyor. Hazırlama ve V2 reprepare, makine programında bulunan veya mevcut sessiz C hazırlığından alınan adları kullanıyor. Legacy step, hazır statement adlarını koruyor. [C kaynak ve uygulama](/github/sqliters/target/perf-work/select-column-names-independent/sonuc.md)

168 normal akış C ile iki kipte ham eş. 30 yaşam akışının tüm metadata anları eş. 15 V2 yaşam akışının ham kaydı eş. Önceki 48 ad bayrağı vakasında C eşliği sağlandı ve 444 önceki kaynak adı, WHERE ve ORDER BY akışı ham olarak korundu. 3.450 salt okunur C dosya açılışında SHA-256 ve içerik eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/select-column-names-independent/res/proof.sha256)

Birleşik optimize aday, 387/400 OFF ve 400/400 ON ile önceki tam DML özetlerini verdi. [Tam korpus](/github/sqliters/target/perf-work/select-column-names-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,44 ms taban, 376,15 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/select-column-names-disk-verify/res/gates-validation.txt).
