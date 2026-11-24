# V6b.562 — Altı fiziksel INNER JOIN kaynağının hazırlanmış bağlaması

174 hedef fark kapandı ve 90 doğru kontrol korundu. 264/264 C yaşam döngüsü, 132/132 gerçek Tcl V1, 4.032 C yeniden açılışı ve önceki 1.092 ham kayıt eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/six-table-inner-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v562-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1436 geçti, 0 başarısız, 319 yok sayıldı. ON 1436 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,79 ms taban, 385,31 ms aday. Süre oranı 1,285 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/six-source-inner-join-binding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
