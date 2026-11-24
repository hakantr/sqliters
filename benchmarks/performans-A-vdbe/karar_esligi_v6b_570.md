# V6b.570 — Altı fiziksel kaynakta iki LEFT ve üç INNER JOIN

198 fark kapandı ve 138 doğru kontrol korundu. 336/336 C yaşam döngüsü, 168/168 Tcl V1 ve önceki 936 ham kayıt eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/six-table-two-left-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v570-with-v548-correction-proof/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1444 geçti, 0 başarısız, 318 yok sayıldı. ON 1444 geçti, 0 başarısız, 318 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,74 ms taban, 389,52 ms aday. Süre oranı 1,300 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/six-table-two-left-reprepare-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
