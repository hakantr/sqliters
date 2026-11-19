# V6b.127 — Karma indeks kolasyonunun INDEX_XINFO gösterimi

Ana şemadaki rowid tablonun tam, benzersiz olmayan, tek doğrudan sütunlu indeksinde doğrudan ve tablo değerli INDEX_XINFO, dış yerleşik kolasyonun SQL'deki özgün yazımını gösteriyor. Dolu 18 görüntüde her Rust kipindeki 48 metadata farkı kapandı. Her kipte 254/254 ölçüm C ile eş. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v125-mixed-index-xinfo-independent.tGAX11Is/sonuc.md)

Korpus üç kodlamada 108 salt okunur yeniden açılış ve tırnaklı veya parantezli 14 yeni kontrol içeriyor. 91 veritabanı görüntüsünün SHA değerleri korundu. Birleşik DML OFF 390/400 ve ON 400/400. [Tam kaynak ve DML manifesti](/github/sqliters/target/perf-work/mixed-index-xinfo-final-release-proof/res/proof.sha256)

Aynı karşılaştırma V126 pager değişikliğini içeren exact V127 release ikilisiyle tekrarlandı. Özet ve bütün gözlem sayaçları bağımsız adayla aynı. [Birleşik tekrar](/github/sqliters/target/perf-work/v127-combined-replay.4z3se3fV/replay-files.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 474,78 ms taban, 444,93 ms aday. Süre oranı 0,937 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-xinfo-disk-verify/res/gates-validation.txt).
