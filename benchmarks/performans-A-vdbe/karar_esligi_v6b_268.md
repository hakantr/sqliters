# V6b.268 — Tamsayı aggregate plan testi

Mevcut testin sekiz profili, profil başına 700 işlemi ve tüm kıyasları iki workspace ortamında aynı gövdeyle çalışır. Üretim yolu için 48 normal COUNT ve SUM okuması, 48 C salt okunur açılışı ve ham tekrar eşliği doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v269-int-kernel-test-routing-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,63 ms taban, 374,84 ms aday. Süre oranı 1,251 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/int-kernel-test-routing-disk-verify/res/gates-validation.txt).
