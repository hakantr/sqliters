# V6b.551 — Uygulamanın RTRIM karşılaştırıcısında hazırlık ve sıralama

Her kipte RTRIM ve önceki NOCASE için 1.728 tam sıralama, 1.728 top-N, 54 fiziksel PMA ve 324 hata sonrası reset eş. V1 adım/reset/finalize ve V2 yeni karşılaştırıcı C ile eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/rtrim-override-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v551-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1425 geçti, 0 başarısız, 320 yok sayıldı. ON 1425 geçti, 0 başarısız, 320 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 302,04 ms taban, 390,16 ms aday. Süre oranı 1,292 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/rtrim-application-sort-replacement-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
