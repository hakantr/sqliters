# V6b.288 — WITHOUT ROWID görünümü iki tam indeks

Kapsayıcı olmayan iki sıradan tam BINARY ikincil indeksli WR görünüm aralığı, C ile eş kökü seçer. 360 eski log veya dosya farkı kapandı. 1.632 iki indeks profili ve 7.728 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v283-view-update-wr-two-index-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 307,83 ms taban, 387,81 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-two-index-disk-verify/res/gates-validation.txt).
