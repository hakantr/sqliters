# V6b.506 — Salt okunur uygulama başlığı yazıcısında ilk adım ve reset

64 native karşılaştırma, 96 C ve 192 Rust açılışı eş. V1 ilk adım 1, ilk reset 8 ve sonraki reset 17 ile önceki tanıyı korur. V2 yeniden hazırlama eş. Group64 36/36, Group65 40/40 ve V505 sıralama kontrolü 978/978. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v506-readonly-header-rebase/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v506-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1376 geçti, 0 başarısız, 324 yok sayıldı. ON 1376 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,82 ms taban, 384,55 ms aday. Süre oranı 1,283 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/readonly-header-pragma-diagnostic-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
