# V6b.299 — WITHOUT ROWID INTEGER ve REAL indeks koşulu

INTEGER sorgu ve çıplak REAL kısmi koşulunda C’nin tam indeks seçimi uygulanır. 160 eski, 320 taze ve 80 ek token sonucu eş. 21.768 C salt okunur bütünlük kontrolü geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v291-view-update-wr-same-real-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 557,68 ms taban, 684,08 ms aday. Süre oranı 1,227 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-same-real-ineligible-partial-disk-verify/res/gates-validation.txt).
