# V6b.380 — Eski satırda sütun varsayılanının metin kodlaması

Sonradan eklenmiş TEXT DEFAULT sütununu eski fiziksel kayıttan okuyan VDBE, metni bağlantının veritabanı kodlamasına getiriyor. 324 gözlemde 16 C farkı kapandı. Önceki DISTINCT geçişinin 24 Rust sonucu korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v380-column-default-encoding-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1258 geçti, 1 başarısız, 325 yok sayıldı. ON 1255 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,23 ms taban, 380,33 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/column-default-encoding-disk-verify/res/gates-validation.txt).
