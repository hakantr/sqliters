# V6b.444 — Doğrudan function_list yetkilendirme ve şema tanısı

Altı şema biçimi, dört yetkilendirme seçimi, üç kodlama ve V1/V2 üzerinden 288 akış ve 1.248 API kaydı C ile eş. Geçersiz şema, callback öncesi SQL ofsetiyle reddedilir. IGNORE sıfır satır ve sütunla reset yaşam döngüsünü korur. [Bağımsız kanıt](/github/sqliters/target/perf-work/v444-function-list-authorizer-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1323 geçti, 1 başarısız, 325 yok sayıldı. ON 1321 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,67 ms taban, 383,67 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/direct-function-list-authorization-disk-verify/res/gates-validation.txt).
