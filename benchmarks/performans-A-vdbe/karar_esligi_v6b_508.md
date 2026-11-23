# V6b.508 — Hazır TEXT bağının encoding bilgisini koruma

UTF-8, UTF-16LE/BE, iki slot ve limitli/varsayılan yürütmede 116 native C eş. V507’nin 24 rota ve 12 bind kontrolü, Group65 45/45, Group64 36/36. Bağlama ayarı tek atomik görüntü olarak yayımlanır. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v508-bind-encoding-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v508-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1381 geçti, 0 başarısız, 324 yok sayıldı. ON 1381 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 301,49 ms taban, 386,05 ms aday. Süre oranı 1,280 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/bind-text-encoding-stability-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
