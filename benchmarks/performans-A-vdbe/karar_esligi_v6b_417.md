# V6b.417 — PMA açmayan özel adlı FILE sıralaması

Sabit C varsayılan PMA tabanı altında kalacağı kayıt baytlarından doğrulanan FILE sıralaması aynı karşılaştırıcı sırasını kullanıyor. Her Rust kipinde 136 profil C ile eşleşti ve 112 karşılaştırıcı dizisi kapandı. Tam callback argümanları, zorlanan tahsis ve gerçek C PMA açma denetimleri ile 180 olağan ertelenmiş yabancı anahtar çalıştırması doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v417-file-sorter-bound-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1297 geçti, 1 başarısız, 325 yok sayıldı. ON 1295 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 296,71 ms taban, 376,43 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/file-sorter-bound-disk-verify/res/gates-validation.txt).
