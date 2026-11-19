# V6b.111 — WITHOUT ROWID görünüm UPDATE alt ve üst sınırı

Doğrudan WITHOUT ROWID görünümünde aynı açık ikincil sayısal indeksin iki literal sınırı mevcut aralık yürütmesinde işleniyor. C geçici tablo materyalizasyonu ve gerçek INSTEAD OF UPDATE log sırası kullanıldı. [Kaynak ve fiziksel kanıt](/github/sqliters/target/perf-work/view-update-wr-two-bounds-independent/sonuc.md)

Üç kodlamada log ve dosya SHA OFF 24/72'den 72/72'ye, ON 40/72'den 72/72'ye çıktı. 24 WR tek aralık ve 36 rowid iki sınır kontrolü C eş, salt okunur C açılışları doğrulandı. Birleşik optimize DML OFF 390/400, ON 400/400 ve önceki ham özetle birebir. [Odak manifesti](/github/sqliters/target/perf-work/view-update-wr-two-bounds-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/view-update-wr-two-bounds-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,38 ms taban, 377,87 ms aday. Süre oranı 1,250 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-two-bounds-disk-verify/res/gates-validation.txt).
