# V6b.297 — WITHOUT ROWID farklı üst eşik

C’nin farklı işlemciyle elediği aynı anahtarlı kısmi indeks yerine tam indeksin doğru üst sınırı kullanılacak. 80 eski tarama kökü ve 320 taze ilişki sonucu C ile eş. 18.552 salt okunur C bütünlük kontrolü geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v289-view-update-wr-lt-le-boundary-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 313,23 ms taban, 401,74 ms aday. Süre oranı 1,283 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-lt-le-boundary-disk-verify/res/gates-validation.txt).
