# V6b.300 — WITHOUT ROWID farklı anahtarlı REAL koşulu

Farklı ilk sütunlu kısmi indeksin çıplak REAL koşulu C gibi maliyet öncesinde elenir. 160 eski ve 200 ek sonuç C ile aynı. 23.928 C salt okunur bütünlük kontrolü geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v292-view-update-wr-alternate-real-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,68 ms taban, 385,93 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-alternate-real-ineligible-partial-disk-verify/res/gates-validation.txt).
