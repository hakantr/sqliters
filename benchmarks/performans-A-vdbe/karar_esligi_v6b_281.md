# V6b.281 — Görünüm UPDATE düşük boşluklu yuvalar

İki gerçek V2 bağ yuvası 1–31 içindeyse kısmi equality kökü ve maskesi C ile eş. Önceki 68 dosya farkı kapandı. 16 normal okumadan yazmaya geçiş örneğinde üçüncü STEP kök ve log eş, ilk fark açık. 9.384 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v278-view-update-sparse-low-bound-equality-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 305,45 ms taban, 392,52 ms aday. Süre oranı 1,285 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-sparse-low-bound-equality-disk-verify/res/gates-validation.txt).
