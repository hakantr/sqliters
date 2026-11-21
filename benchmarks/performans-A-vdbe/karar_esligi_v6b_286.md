# V6b.286 — WITHOUT ROWID görünümü adlandırılmış RETURNING

Tek sıradan BINARY ikincil indeksli WITHOUT ROWID görünümünde nitelemesiz RETURNING ile sayı aralığı C materyalizasyon sırasını kullanır. Önceki 120 fark kapandı ve 480 DELETE veya WAL hedefi C eş. 2.592 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v281-view-update-wr-explicit-returning-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,00 ms taban, 385,81 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-explicit-returning-disk-verify/res/gates-validation.txt).
