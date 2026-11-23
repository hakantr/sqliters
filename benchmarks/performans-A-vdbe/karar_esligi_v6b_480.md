# V6b.480 — WITHOUT ROWID BINARY PK sıralamasında işçi PMA

Aynı tablonun packed fiziksel birincil anahtarı doğrulandı. Kip başına 144 profil, 73.944 C adımı, 72 ilk satır reseti ve 96.530 VFS olayı eş. Önceki 60 guard ve altı fiziksel FK kontrolü geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-without-rowid-pk-rebased-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v480-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1350 geçti, 0 başarısız, 325 yok sayıldı. ON 1350 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,65 ms taban, 387,76 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/without-rowid-primary-key-worker-pma-disk-verify/res/gates-validation.txt).
