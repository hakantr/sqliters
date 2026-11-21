# V6b.330 — Sıralamasız UNION ve sabit LIMIT 1

İki olağan koldaki sıralamasız UNION sabit tamsayı LIMIT 1 ile kolları kaynak sırasıyla değerlendirir. 120 fark kapandı. 25 sorgu biçimi ve 18 C salt okunur açılış doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v330-union-literal-limit-one-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1206 geçti, 1 başarısız, 329 yok sayıldı. ON 1203 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,27 ms taban, 381,64 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/union-literal-limit-one-disk-verify/res/gates-validation.txt).
