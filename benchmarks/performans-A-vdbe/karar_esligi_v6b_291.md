# V6b.291 — WITHOUT ROWID görünümü aynı anahtarlı kısmi indeks

Aynı BINARY anahtardaki yapısal olarak eş tamsayı kısmi koşulu, C ile eş materyalizasyon kökünü kullanır. 384 hedefte 640 önceki ilişkisel fark kapandı. 8.040 C salt okunur açılışı ve kapsayıcı durumlar doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v285-view-update-wr-same-key-partial-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 348,89 ms taban, 464,09 ms aday. Süre oranı 1,330 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-same-key-partial-disk-verify/res/gates-validation.txt).
