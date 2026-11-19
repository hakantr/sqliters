# V6b.121 — Ortak journal ve desteklenen disk slotları

Mevcut ortak statement sahibi ve ileri commit, iki veya daha fazla uygun NORMAL disk slotunu kapsıyor. Dört ve altı disk ile MAIN ve on ATTACH üst sınırı C ile doğrulandı. Gerçek önkoşullar disk ve pager durumuna bağlı. [Kaynak ve sınır kanıtı](/github/sqliters/target/perf-work/v119-journal-many-slots-independent/sonuc.md)

Birleşik exact aday 180 dört/altı, 6 on bir ve 180 iki/üç native akışta C eş. 11.472 ana SHA, 4.302 C salt okunur açılış ve 966 ROW journal checksum. Birleşik optimize DML OFF 390/400, ON 400/400 ve önceki ham özetle eş. [Exact bağımsız kanıt](/github/sqliters/target/perf-work/v121-journal-many-slots-independent/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/journal-many-slots-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,71 ms taban, 377,48 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/journal-many-slots-disk-verify/res/gates-validation.txt).
