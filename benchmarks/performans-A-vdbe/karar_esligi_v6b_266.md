# V6b.266 — Görünüm UPDATE sabit eşitlik ve aralık

Açık MAIN yazma işleminde ayrı INTEGER eşitlik ve NUMERIC aralık sütunlarını C yapısal uygunluğuna göre mevcut planlayıcıya verir. 1.152 DELETE/WAL hedefi, 168 ek sözdizimi hedefi ve 14.256 salt okunur C açılışı eş. 192 unary artı komşusu eski davranışı korudu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v266-view-update-literal-equality-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,36 ms taban, 379,12 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-literal-equality-disk-verify/res/gates-validation.txt).
