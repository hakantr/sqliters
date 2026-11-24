# V6b.571 — Boş dosyada WAL başlığının ve ilk sayfanın yayımlanması

Beş başlangıç kipinde 180 EXCLUSIVE farkı kapandı. C ile 360 yaşam döngüsü karşılaştırmasında yan dosya, ham ana dosya ve salt okunur açılış eş. Locking mode testleri iki kipte 10/10. [Bağımsız ölçüm](/github/sqliters/target/perf-work/e28-empty-memory-wal-staged-candidate.kect0IEp/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v571-with-v548-correction-proof/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1445 geçti, 0 başarısız, 318 yok sayıldı. ON 1445 geçti, 0 başarısız, 318 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 297,04 ms taban, 385,96 ms aday. Süre oranı 1,299 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/empty-named-wal-staged-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
