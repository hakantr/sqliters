# V6b.503 — Uygulama metadata yazıcısında V1 reset ve V2 yeniden hazırlama

320 native karşılaştırma, 128 tanı izi ve 480 C ile 960 Rust yeniden açılışı eş. Başarılı yazıcıda V1 reset ve finalize önceki tanıyı korur, V2 bir kez yeniden hazırlanır. FK boş, integrity_check ok. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v503-header-lifecycle-rebase/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v503-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1373 geçti, 0 başarısız, 324 yok sayıldı. ON 1373 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 304,56 ms taban, 388,57 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/application-header-prepare-reset-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
