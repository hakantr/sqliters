# V6b.338 — LIMIT ile WITHOUT ROWID kapsayan tam tarama

Filtresiz ve sıralamasız tek tablo okumalarında LIMIT ve OFFSET C ile aynı kapsayan taramayı seçiyor. 54 profilde 590 fark kapandı ve 162 C yeniden açılışı doğrulandı. Sekiz odaklı C ve Rust kontrolü geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v338-wr-covering-limit-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1214 geçti, 1 başarısız, 329 yok sayıldı. ON 1211 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 558,83 ms taban, 722,18 ms aday. Süre oranı 1,292 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wr-covering-limit-disk-verify/res/gates-validation.txt).
