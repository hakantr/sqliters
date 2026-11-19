# V6b.146 — İfade indeksinde INDEX_XINFO kolasyon adı

Uygun MAIN rowid tablonun tam non-UNIQUE ifade indeksinde, C ifade kökündeki açık yerleşik kolasyonun özgün yazımını gösterir. Diğer ifade köklerinde BINARY gösterilir. Doğrudan PRAGMA ve tablo değerli metadata aynı kararı kullanır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v143-expression-index-xinfo.CR2rcBgS/sonuc.md)

18 arşiv farkı kapandı, 108 önceki açılış eş kaldı. Altı sağlıklı dosyada her Rust kipinde 726/726 C sonucu, 11 ayrıştırıcı testi ve 2.755 strict kayıt doğrulandı. DB ve yan dosya SHA değerleri korundu. [Strict metadata kanıtı](/github/sqliters/target/perf-work/v143-expression-index-xinfo.CR2rcBgS/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/expression-index-xinfo-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 305,21 ms taban, 383,56 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/expression-index-xinfo-disk-verify/res/gates-validation.txt).
