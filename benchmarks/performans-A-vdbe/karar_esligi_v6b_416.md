# V6b.416 — Örtük şema kolasyonu bellek sıralaması

Şemadan gelen NOCASE, RTRIM ve uygulama kolasyonu tam bellek sıralamasında her Rust kipinde 222 profil ve 15.648 adım boyunca C ile eşleşti. Unicode ve NUL için 216 ek kayıt, özgün hata ve 18 DELETE/WAL dosyası doğrulandı. Aynı kaynakla 180 olağan ertelenmiş yabancı anahtar çalıştırması da C ile eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v417-implicit-schema-collation-memory-sort-preflight/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1296 geçti, 1 başarısız, 325 yok sayıldı. ON 1294 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,16 ms taban, 377,86 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/implicit-schema-collation-memory-sort-disk-verify/res/gates-validation.txt).
