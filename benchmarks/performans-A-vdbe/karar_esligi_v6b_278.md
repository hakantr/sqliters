# V6b.278 — Görünüm UPDATE bağlı aralık

Açık MAIN V2 görünüm UPDATE yeniden hazırlamasında dar bağlı NUMERIC aralık ve plain INTEGER kısmi predikat C yapısal karşılaştırmasını kullanır. 456 STEP kökü, 256 son dosya ve 5.476 C açılışı eş. Önceki eşitlik ve aynı tamsayı vakaları korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v276-view-update-bound-range-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,30 ms taban, 383,04 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-bound-range-disk-verify/res/gates-validation.txt).
