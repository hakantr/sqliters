# V6b.99 — Görünüm UPDATE ve scalar MAX eşitliği

Aynı normal görünümü okuyan, yerleşik scalar MAX ve tek uyumlu bileşik indeks kapsamı için C'nin iki ayrı değerlendirme ömrü uygulanıyor. Geçici görünüm taramasında ilk değer kullanılıyor. Özgün WHERE, geçici satır varsa bağımsız ikinci değerle değerlendiriliyor. Yerleşik MIN/MAX kodlama düzeltmesi V6b.98'den geliyor. [C kaynak ve bağımsız kanıt](/github/sqliters/target/perf-work/view-max-v98-independent/sonuc.md)

V1 ve V2'nin her birinde ham eşlik OFF 36/123'ten 96/123'e, ON 65/123'ten 105/123'e çıktı. Kalıcı log, dosya SHA ve C açılışı OFF 39/123'ten 96/123'e, ON 67/123'ten 105/123'e çıktı. Ana tablo 123/123. 60 değerlendirme izi, reset ve iki bağımsız cache sınırını doğruladı. Önceki 282 DELETE sonucu ve açık aralık farkları korundu. [Bağımsız SHA](/github/sqliters/target/perf-work/view-max-v98-independent/sha256.txt)

Birleşik optimize adayda OFF 388/400 ve ON 400/400. Ham DML özetleri V6b.98 ile aynı. [Tam korpus](/github/sqliters/target/perf-work/view-max-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 388/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,13 ms taban, 375,67 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-max-disk-verify/res/gates-validation.txt).
