# V6b.113 — Tek hedefli journal modu ilk ROW ve commit

Tek dosyalı ya da açık şema nitelikli sağlıklı DELETE↔WAL SQL geçişi, ilk ROW sırasında önceki ana dosya başlığını ve aktif koruma journalını korur. Normal DONE, reset, finalize ya da Drop mevcut statement sahibi ve commit koordinatörüyle geçişi bir kez tamamlar. [Kaynak ve yaşam sözleşmesi](/github/sqliters/target/perf-work/single-journal-mode-lifecycle-fix-independent/sonuc.md)

Birleşik exact V113 için 180/180 native, 48 sahiplik ve 12 doğrudan Pager/Btree kontrolü. 2.592 ana SHA, 228 aktif jurnal checksum ve 660 C salt okunur açılışı eş. Birleşik optimize DML OFF 390/400 ve ON 400/400, önceki ham özetle birebir. [Exact C karşılaştırması](/github/sqliters/target/perf-work/v113-single-journal-lifecycle-independent/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/single-journal-mode-lifecycle-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,60 ms taban, 375,80 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/single-journal-mode-lifecycle-disk-verify/res/gates-validation.txt).
