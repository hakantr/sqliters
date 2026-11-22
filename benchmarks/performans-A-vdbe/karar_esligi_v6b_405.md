# V6b.405 — Skaler doğruluk projeksiyonunda kayıtlı kolasyon

Karma IS TRUE projeksiyonlarında önceki altı fark kapandı. Her Rust kipinde 1.374 C kayıt ve 33 projeksiyon kontrolü eş. Önceki 696 geri çağrı kaydı korundu. Group64 6/6, hata/reset ve sağlıklı dosya açılışları geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v405-row-collation-truth-projection-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1285 geçti, 1 başarısız, 325 yok sayıldı. ON 1283 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,56 ms taban, 380,49 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/collation-truth-projection-disk-verify/res/gates-validation.txt).
