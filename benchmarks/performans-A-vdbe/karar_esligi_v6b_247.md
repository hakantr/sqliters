# V6b.247 — Görünüm UPDATE eş maliyetli iki tam indeks

Sabit şemada aynı BINARY NUMERIC anahtarını kullanan iki tam aralık indeksi, tablo okuması gerektiğinde ve mevcut MAIN okuma veya yazma snapshot’ı varsa C katalog sırasıyla değerlendirilir. 1.060 kalıcı sıra ve dosya farkı kapandı. Önceki 644 vaka, 48 covering kontrolü ve 6.276 bağımsız C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v246-view-update-competing-full-index-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 554,73 ms taban, 707,74 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-competing-full-index-disk-verify/res/gates-validation.txt).
