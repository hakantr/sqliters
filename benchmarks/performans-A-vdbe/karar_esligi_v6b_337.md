# V6b.337 — Yabancı anahtarlı WITHOUT ROWID kaynağında kapsayan indeks

Kapsama hesabı yabancı anahtar tanımlı WITHOUT ROWID kaynaklarında C ile aynı taramayı seçiyor. 54 profilde 420 fark kapandı ve 162 C salt okunur yeniden açılışı doğrulandı. DML OFF 6189 kapandı, kalan sekiz ham kayıt aynı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v337-wr-covering-fk-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1213 geçti, 1 başarısız, 329 yok sayıldı. ON 1210 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 307,67 ms taban, 388,27 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wr-covering-fk-disk-verify/res/gates-validation.txt).
