# V6b.550 — Dört ayrı şemalı hazırlanmış INNER JOIN

144 gerçek fark kapandı, 72 doğru kontrol korundu. C yaşam döngüsü 216/216, Tcl V1 108/108, 3.384 C disk açılışı ve 960 önceki ham kayıt eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/four-schema-inner-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v550-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1424 geçti, 0 başarısız, 320 yok sayıldı. ON 1424 geçti, 0 başarısız, 320 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,16 ms taban, 384,77 ms aday. Süre oranı 1,286 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/four-schema-inner-join-binding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
