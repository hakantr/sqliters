# V6b.473 — Biricik rowid ilk veya orta anahtardayken işçi PMA

Toplam BINARY sıralamada rowid aynı tek tablo cursor’ının packed anahtar önekinde herhangi bir yerde olabilir. Her kipte 144 profil, 221.400 C adımı ve aynı ifadenin 72 ilk satır reseti doğrulandı. Altı dosyada FK ve bütünlük korundu. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-rowid-key-position-preflight/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v473-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1345 geçti, 0 başarısız, 325 yok sayıldı. ON 1345 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 306,69 ms taban, 389,95 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/rowid-position-worker-pma-disk-verify/res/gates-validation.txt).
