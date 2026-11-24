# V6b.566 — EXCLUSIVE MEMORY ve WAL geçişinin sonuç satırı sınırı

72 C ve her kipte 36 Rust yaşam döngüsü doğrulandı. 96 geçişte SQL, C yeniden açılışı, ana dosya ve journal açma sayısı eş. 48 disk journal kontrolü ve altı aktif okuyucu ile yeniden deneme geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/e28-exclusive-memory-wal-staged-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v566-with-v548-correction-proof/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1440 geçti, 0 başarısız, 319 yok sayıldı. ON 1440 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 298,82 ms taban, 387,44 ms aday. Süre oranı 1,297 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/exclusive-memory-wal-staged-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
