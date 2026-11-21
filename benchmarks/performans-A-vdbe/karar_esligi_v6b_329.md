# V6b.329 — UNION eşdeğer satır temsili

Materyalize UNION aynı kolda ilk temsilciyi korur ve eşitlikte sağ kolu seçer. 120 değer ve tür farkı kapandı. 27 sorgu biçimi, 18 C salt okunur açılış ve altı Rust kontrolü doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v329-union-merge-representatives-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1205 geçti, 1 başarısız, 329 yok sayıldı. ON 1202 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,59 ms taban, 378,57 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/union-merge-representatives-disk-verify/res/gates-validation.txt).
