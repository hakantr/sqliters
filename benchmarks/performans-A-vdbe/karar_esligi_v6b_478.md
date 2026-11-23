# V6b.478 — Tek işçide ilk adımdan önce etkin PMA eşlemesi

Kip başına 84 API profili, 48 zorlanmış eş VFS kaydı, ilk xOpen 14, sınırlar ve gerçek görev sırası doğrulandı. Altı dosyada FK, bütünlük ve SHA eş. İlk satır, reset ve tekrar yürütme iki kipte Group64 28/28. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-initial-mmap-rebased-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v478-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1348 geçti, 0 başarısız, 325 yok sayıldı. ON 1348 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,87 ms taban, 383,07 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/one-worker-initial-pma-mmap-disk-verify/res/gates-validation.txt).
