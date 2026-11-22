# V6b.411 — Çok kollu ikincil indeks OR

Tüm ikincil kollar aynı tam bileşik anahtarı ve ilk, orta, son akışını kullanıyor. Dört, beş, altı ve sekiz kollu C biçimlerinde 208 UTF-16 sonucu ve 312 tam EXPLAIN planı eşleşti. Önceki iki ve üç kollu sonuçlar ile birincil ve ikincil iki kollu kontrol korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v411-row-case-many-secondary-or-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1291 geçti, 1 başarısız, 325 yok sayıldı. ON 1289 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 307,77 ms taban, 389,76 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-many-secondary-or-disk-verify/res/gates-validation.txt).
