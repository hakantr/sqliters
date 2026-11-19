# V6b.116 — Çoklu hedef journal modu ortak commit

Nitelemesiz sağlıklı iki veya üç disk hedefli DELETE↔WAL geçişinde C’nin ters slot hazırlığı ve ilk ROW sahipliği uygulanıyor. Normal bitişte mevcut koordinatör sayfa 1'i ileri slot sırasıyla yazıyor. WAL→DELETE, C gibi ortak rollback journal kullanıyor. [Kaynak, yazım ve yaşam kanıtı](/github/sqliters/target/perf-work/multi-journal-mode-lifecycle-fix-independent/sonuc.md)

Birleşik V116, 108 çoklu ve 180 tek/nitelikli native akışta C eş. 72 sahiplik, 36 sağlıklı sistem çağrısı akışı ve 1.692 C salt okunur açılışı doğrulandı. 6.228 ana SHA, 588 ilk ROW journal preimage. Önceki 32 fiziksel sıra farkının 24'ü kapandı. Birleşik optimize DML OFF 390/400 ve ON 400/400, önceki ham özetle eş. [Exact C karşılaştırması](/github/sqliters/target/perf-work/v116-multi-journal-lifecycle-independent/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/multi-journal-mode-lifecycle-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,79 ms taban, 376,05 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/multi-journal-mode-lifecycle-disk-verify/res/gates-validation.txt).
