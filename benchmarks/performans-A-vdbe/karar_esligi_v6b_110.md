# V6b.110 — Rowid görünüm UPDATE alt ve üst aralığı

Doğrudan rowid görünümünde aynı tam ikincil indeksin iki sayısal literal sınırı, C ile aynı biçimde kaynak SELECT'e taşınıyor. Mevcut aralık yürütmesi ve INSTEAD OF UPDATE log sırası kullanılıyor. C her iki sınırı tek indekste uyguluyor. [C ve Rust kaynak, dosya ve log kanıtı](/github/sqliters/target/perf-work/view-update-between-range-independent/sonuc.md)

Üç kodlamada log ve dosya SHA OFF 12/36'dan 36/36'ya, ON 20/36'dan 36/36'ya çıktı. C salt okunur açılış, 24 önceki tek sınır ve ana tablo sonuçları eş. Birleşik optimize DML 390/400 OFF, 400/400 ON ve önceki ham özetle birebir. [Odak manifesti](/github/sqliters/target/perf-work/view-update-between-range-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/view-update-between-range-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,49 ms taban, 374,85 ms aday. Süre oranı 1,243 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-between-range-disk-verify/res/gates-validation.txt).
