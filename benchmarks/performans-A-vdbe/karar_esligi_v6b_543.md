# V6b.543 — Üç kaynaklı LEFT ve INNER JOIN hazırlanmış kaynakları

216 hedef fark kapandı ve 96 doğru sonuç korundu. 312/312 C yaşam döngüsü, 156/156 Tcl V1, önceki 1.392 ham kayıt ve 3.744 C yeniden açılışı eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/three-table-left-inner-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v543-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1415 geçti, 0 başarısız, 324 yok sayıldı. ON 1415 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 297,38 ms taban, 388,35 ms aday. Süre oranı 1,306 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/three-source-outer-inner-join-binding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
