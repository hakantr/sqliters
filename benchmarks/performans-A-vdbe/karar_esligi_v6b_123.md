# V6b.123 — Karma journal hedefi ve aynı kip slotları

Nitelemesiz journal_mode, mevcut hedef kipteki disk slotlarının dosya takımını koruyor. Bir veya daha fazla gerçek geçiş, mevcut ortak statement sahibiyle ilk ROW ve doğal bitişi takip ediyor. WAL çıkışında ortak journal yalnız gerçek katılımcıları izliyor. [C kaynak ve kayıtlar](/github/sqliters/target/perf-work/v121-mixed-target-journal-independent/sonuc.md)

Birleşik exact aday 234 yeni ve 366 önceki sağlıklı akışta C eş. 522 aynı kip slotunun baytları ve inode'u sabit. 18.816 ana SHA, 7.056 C salt okunur açılış ve 468 ham syscall kaydı doğrulandı. Birleşik optimize DML OFF 390/400, ON 400/400 ve önceki ham özetle aynı. [Exact bağımsız ölçüm](/github/sqliters/target/perf-work/v123-mixed-target-journal-independent/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/journal-mode-mixed-target-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,06 ms taban, 378,34 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/journal-mode-mixed-target-disk-verify/res/gates-validation.txt).
