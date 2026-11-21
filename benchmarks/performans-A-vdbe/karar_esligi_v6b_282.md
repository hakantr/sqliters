# V6b.282 — Görünüm UPDATE yüksek bağ yuvaları

C'nin paylaşılan yüksek bağ biti gerçek equality yuvasından ayrı kullanılır. Önceki matrisin 68 son dosya farkı kapandı. 12.436 C salt okunur açılışı ve sonraki STEP kök/log kararları doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v279-view-update-sparse-high-bound-equality-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 307,69 ms taban, 390,22 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-sparse-high-bound-equality-disk-verify/res/gates-validation.txt).
