# V6b.514 — Kayıtlı FILE ve DEFAULT sıralamasında 3–8 gerçek işçi

3–8 işçi, 288 profil ve üç tekrar. Her kipte 864/864 C satır, callback, SORT ve reset sonucu eş. Sınır clamp 24/24 ve küçük sorgu 96/96. Aynı PMA ataması görülen 145/145 callback dağılımı eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/registered-collation-worker-limit-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v514-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1387 geçti, 0 başarısız, 324 yok sayıldı. ON 1387 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,41 ms taban, 390,40 ms aday. Süre oranı 1,287 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/registered-pma-worker-limit-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
