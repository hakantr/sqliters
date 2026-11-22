# V6b.368 — Tcl karşılaştırma işlevi ve sonlandırma geri çağrısı

C ve iki Rust kipinde eski collate7 kapsamı ve üç kodlamadaki 24 ek yaşam döngüsü kontrolü geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v368-tcl-collation-destructor-corrected-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1245 geçti, 1 başarısız, 328 yok sayıldı. ON 1242 geçti, 4 başarısız, 328 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,68 ms taban, 383,46 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/tcl-collation-destructor-disk-verify/res/gates-validation.txt).
