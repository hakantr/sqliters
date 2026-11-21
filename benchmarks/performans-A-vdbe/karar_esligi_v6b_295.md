# V6b.295 — WITHOUT ROWID farklı anahtarlı kısmi indeks

C’nin farklı eşikte elediği kısmi indeks yerine tam indeksin aynı aralığı kullanılıyor. Önceki 800 dosya farkının 160’ı kapandı. Taze kontrolle 40 ek sonuç ve 15.672 C salt okunur açılış eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v288-view-update-wr-alternate-ineligible-partial-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,84 ms taban, 382,58 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-alternate-ineligible-partial-disk-verify/res/gates-validation.txt).
