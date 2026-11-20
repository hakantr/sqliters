# V6b.199 — TEMP CREATE hazırlığı ve şema cookie ömrü

Başarılı CREATE TEMP TABLE hazırlığı TEMP Btree’yi açıyor. TEMP depolaması değişince eski şema cookie’si ilk gerçek işlem kontrolüne kadar saklanıyor. V1’de stale şema ile hedef tablo oluşmuyor. V2 deyimi yeniden hazırlayıp tabloyu oluşturuyor. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v198-temp-create-cookie-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 305,19 ms taban, 384,29 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-create-schema-cookie-disk-verify/res/gates-validation.txt).
