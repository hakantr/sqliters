# V6b.388 — UTF-16 satır CASE anahtarları

558 gözlemli exact karşılaştırmada son 48 fark kapandı. FIFO UNION, BINARY TOP-N LIMIT/OFFSET, NOCASE fiziksel arama ve NOCASE/RTRIM DISTINCT düzeltildi. İki kipte 372/372 Rust sonucu ve altı C salt okunur dosya açılışı eş. Daha geniş 1.188 gözlemde önceki eş sonuçlar korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v388-row-case-key-shapes-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1269 geçti, 1 başarısız, 325 yok sayıldı. ON 1266 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 411,33 ms taban, 497,44 ms aday. Süre oranı 1,209 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-key-shapes-disk-verify/res/gates-validation.txt).
