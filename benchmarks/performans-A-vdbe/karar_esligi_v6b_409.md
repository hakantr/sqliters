# V6b.409 — Kayıtlı NOCASE ve RTRIM bellek sıralaması

Açık NOCASE ve RTRIM ile tam bellek sıralamasında uygulama karşılaştırıcıları her Rust kipinde 360 profil ve 25.416 adım boyunca C ile eşleşti. Aynı deyimde FILE ve MEMORY geçişi, callback argüman sırası ve özgün hata aktarımı doğrulandı. 72 kapsam dışı sıralama profili önceki davranışını korudu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v409-builtin-collation-memory-sort-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1289 geçti, 1 başarısız, 325 yok sayıldı. ON 1287 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,85 ms taban, 379,96 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/builtin-collation-memory-sort-disk-verify/res/gates-validation.txt).
