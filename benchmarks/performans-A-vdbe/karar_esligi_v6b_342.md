# V6b.342 — reverse_unordered_selects ile olağan CROSS JOIN

İki indekssiz MAIN rowid kaynağı arasındaki filtresiz CROSS JOIN, reverse_unordered_selects açıkken kaynakları C sırasıyla okuyor. 20 fark kapandı. 18 C yeniden açılışı ve altı odaklı kontrol doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v342-reverse-unordered-cross-join-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1218 geçti, 1 başarısız, 329 yok sayıldı. ON 1215 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 309,81 ms taban, 392,00 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/reverse-unordered-cross-join-disk-verify/res/gates-validation.txt).
