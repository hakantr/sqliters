# V6b.303 — WITHOUT ROWID görünümünde çift eksi koşulu

Aynı anahtardaki iki unary eksi koşulunda C’nin tam aralık kökü kullanılır. 160 eski ve 520 ek sonuç C ile eş. 29.352 C salt okunur bütünlük kontrolü tamamlandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v294-view-update-wr-same-folded-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,10 ms taban, 383,37 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-same-folded-ineligible-partial-disk-verify/res/gates-validation.txt).
