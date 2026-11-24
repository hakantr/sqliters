# V6b.548 — Unary artı ile sütun kolasyonu ve alias

Üç kodlamada her kipte 54/54 collate8 kapsamı ve 48/48 ek kontrol C ile eş. Unary artı affinity'yi kaldırıp sütun kolasyonunu ve açık COLLATE önceliğini koruyor. [Bağımsız ölçüm](/github/sqliters/target/perf-work/collate8-unary-plus-v542.eorfthil/sonuc.md)

İlk denemede OFF kipinde bulunan in4-4.6 ve in4-5.1 farkları için çalıştırma zamanındaki IN listesi sol ifadenin kolasyonunu kullanıyor. in4 ve planlayıcı denetimi ile unary artı testi iki kipte yeniden geçti. [Düzeltilmiş kaynak ve odak kanıtı](/github/sqliters/target/perf-work/v548-in-list-collation-fix.v3lpPEtD/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1420 geçti, 0 başarısız, 323 yok sayıldı. ON 1420 geçti, 0 başarısız, 323 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 301,54 ms taban, 388,94 ms aday. Süre oranı 1,290 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/unary-plus-collation-inheritance-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
