# V6b.496 — Hazırlanmış rowid yazmalarında SQLite metin baytları

Üç kodlama ve DELETE/WAL için 48 C hazırlık izi, 12 yazma dosyası ve 12 koruma kontrolü eş. Kayıtlı karşılaştırıcı, sonradan açılan blob ve mutation trace mevcut yürütme yolunu kullanır. [Bağımsız ölçüm](/github/sqliters/target/perf-work/text-register-write-exact-verify/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v496-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1366 geçti, 0 başarısız, 324 yok sayıldı. ON 1366 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 302,85 ms taban, 390,97 ms aday. Süre oranı 1,291 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/text-register-rowid-write-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
