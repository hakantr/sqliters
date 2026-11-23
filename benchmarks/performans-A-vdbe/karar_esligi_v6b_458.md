# V6b.458 — Geç VACUUM truncate hatası ve aynı bağlantının yeniden kullanımı

DELETE journal sonlandırıldıktan sonra geç fiziksel kısaltma hata verirse ilk hata korunur. Olağan pager toparlanması tamamlandığında C gibi aynı bağlantı yeniden kullanılabilir. FK, veri, şema ve hazır V2 yeniden hazırlama kontrolü geçti. Önceki hata, sıcak journal ve ikinci temizlik hatası denetimleri korundu. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v458-vacuum-late-truncate-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v458-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1335 geçti, 0 başarısız, 325 yok sayıldı. ON 1335 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,79 ms taban, 385,18 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vacuum-post-commit-truncate-recovery-disk-verify/res/gates-validation.txt).
