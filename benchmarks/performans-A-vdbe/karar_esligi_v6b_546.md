# V6b.546 — Dört fiziksel INNER JOIN kaynağının hazırlanmış şema bağlaması

156 gerçek fark kapandı ve 60 doğru kontrol korundu. 216/216 C yaşam döngüsü, 108/108 gerçek Tcl V1, 2.700 C disk açılışı ve önceki 744 ham kayıt eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/four-table-inner-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v546-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1418 geçti, 0 başarısız, 323 yok sayıldı. ON 1418 geçti, 0 başarısız, 323 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 300,37 ms taban, 387,77 ms aday. Süre oranı 1,291 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/four-source-inner-join-binding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
