# V6b.560 — Beş kaynaklı tek LEFT JOIN hazırlanmış bağlaması

186 hedef fark kapandı, 102 doğru kontrol korundu. 288/288 C yaşam döngüsü, 144/144 gerçek Tcl V1, 5.328 C yeniden açılışı ve önceki 984 ham kayıt eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/five-table-left-inner-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v560-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1434 geçti, 0 başarısız, 319 yok sayıldı. ON 1434 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 300,69 ms taban, 389,77 ms aday. Süre oranı 1,296 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/five-source-left-inner-binding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
