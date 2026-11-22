# V6b.427 — FILE ve DEFAULT LIMIT/OFFSET geçici B-tree akışı

Literal ve bağlı parametreli 576 C profilinde 1.440 adım ve 461.760 VFS okuma ve yazması eşleşti. Açma, okuma ve yazma hataları ile işlem durumu doğrulandı. 48 uygulama hatasından sonra ek G/Ç olmadı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v427-file-topn-transient-preflight/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1307 geçti, 1 başarısız, 325 yok sayıldı. ON 1305 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,12 ms taban, 377,06 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/custom-file-limit-transient-index-disk-verify/res/gates-validation.txt).
