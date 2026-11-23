# V6b.487 — Hazır `unistr` ve `hex` metin baytları

Üç kodlama ve iki journal biçiminde kip başına 12 C API profili, Group65 26/26 ve yeniden etkin func9 15/15. Geçerli FK yazımlarından sonra 12 C/Rust dosya görüntüsü byte eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/builtin-unistr-mem-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v487-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1356 geçti, 0 başarısız, 324 yok sayıldı. ON 1356 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 304,52 ms taban, 383,37 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-unistr-hex-register-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
