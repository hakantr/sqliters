# V6b.279 — strftime Pazartesi başlangıçlı hafta

Açık TEXT biçimi ve tek sabit tarih girdisinde strftime yüzde W, ilk Pazartesiye göre C hafta numarasını üretir. 27 Tcl vakası, üç kodlamada 1.786 hedef ve 18 C salt okunur açılışı eş. Önceki date-19 sonuçları korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v277-strftime-week-monday-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 310,04 ms taban, 393,80 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/strftime-week-monday-disk-verify/res/gates-validation.txt).
