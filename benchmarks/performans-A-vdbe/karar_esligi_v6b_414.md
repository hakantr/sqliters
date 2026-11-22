# V6b.414 — Örtük UTF-8 BINARY bellek sıralaması

Varsayılan veya BINARY tanımlı düz sütunda 132 profil ve 9.264 geri çağrı adımı her Rust kipinde C ile eş. Doğal UTF-16 ve önceki 864 açık sıralama profili korunuyor. Hata sırası, yeniden hazırlama ve 12 C dosyası doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v414-implicit-binary-memory-sort-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1294 geçti, 1 başarısız, 325 yok sayıldı. ON 1292 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 311,24 ms taban, 388,59 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/implicit-binary-memory-sort-disk-verify/res/gates-validation.txt).
