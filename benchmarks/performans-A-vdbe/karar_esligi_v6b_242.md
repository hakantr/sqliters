# V6b.242 — Legacy görünüm UPDATE şema denetimi

Olağan ilişkisel MAIN legacy V1 görünüm UPDATE işlemi gerçek Btree okuma snapshot üstünde hazırlanmış şema kimliğini ve cookie değerini doğrular. 48 OFF ve 32 UTF16 ON geri dönüşünde eski şema yazmadan reddedildi. STEP, RESET, FINALIZE, ana dosya ve log C eş. Ek önceki değişiklik sayacı ve normal takip işlemleri korundu. 576 native ve 576 C açılışı, iki kipte önceki 10/10 test ve 4.735 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v241-legacy-view-schema-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 304,33 ms taban, 381,04 ms aday. Süre oranı 1,252 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-view-schema-disk-verify/res/gates-validation.txt).
