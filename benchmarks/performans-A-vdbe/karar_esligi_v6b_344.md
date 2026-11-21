# V6b.344 — INTEGER PRIMARY KEY eşitliğinde ters JOIN taraması

Nitelikli INTEGER PRIMARY KEY eşitliğinde olağan INNER ve LEFT JOIN, reverse_unordered_selects açıkken C ile aynı satır sırasını veriyor. 60 fark kapandı. 18 C yeniden açılışı ve yedi odaklı kontrol doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v344-reverse-unordered-key-join-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1220 geçti, 1 başarısız, 329 yok sayıldı. ON 1217 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,17 ms taban, 386,42 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/reverse-unordered-key-join-disk-verify/res/gates-validation.txt).
