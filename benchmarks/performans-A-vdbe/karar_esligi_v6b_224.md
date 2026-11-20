# V6b.224 — Açık unixepoch girdisi

İlk geçerli unixepoch değiştiricisi ham sayısal giriş, C Julian yuvarlaması ve iki tarih sınırını kullanıyor. 840 girdide 384 fark ve önceki subsec matrisinde 12 sıra farkı kapandı. 423 C salt okunur açılışı ve 12 VDBE testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v224-unixepoch-input-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 308,98 ms taban, 385,79 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/explicit-unixepoch-input-disk-verify/res/gates-validation.txt).
