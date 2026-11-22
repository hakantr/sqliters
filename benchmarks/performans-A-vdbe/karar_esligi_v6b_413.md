# V6b.413 — Açık UTF-8 BINARY bellek sıralaması

Kayıtlı UTF-8 BINARY karşılaştırıcısı C ile her Rust kipinde 4.236 adım ve 36 Unicode/NUL gözleminde eş. Native UTF-16 sıraları C ile karşılaştırıcısız eşleşti. Karşılaştırma hatasının özgün kodu ve ofseti, aynı deyim FILE/MEMORY geçişi ve altı dosyanın baytları doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v413-explicit-binary-memory-sort-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1293 geçti, 1 başarısız, 325 yok sayıldı. ON 1291 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,00 ms taban, 379,44 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/explicit-binary-memory-sort-disk-verify/res/gates-validation.txt).
