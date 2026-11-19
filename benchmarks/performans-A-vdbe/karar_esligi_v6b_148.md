# V6b.148 — INDEX_XINFO miras kolasyon adı

Uygun MAIN rowid tablonun elle tanımlı tam non-UNIQUE indeksinde, anahtar doğrudan sütundan kolasyon miras alıyorsa C'nin özgün yerleşik ad yazımı gösterilir. Açık indeks kolasyonu önceliklidir. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v143-inherited-index-xinfo.FUoeCUdb/sonuc.md)

Her Rust kipinde 192/192 yeni ve 168/168 metadata formu eş. Önceki 15 dosyada 24 yazım farkı kapandı. 726 eski form ve 108 açılış korundu. Dosya ve yan dosya SHA sabit. [2.760 strict kayıt](/github/sqliters/target/perf-work/v143-inherited-index-xinfo.FUoeCUdb/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/inherited-index-xinfo-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 317,39 ms taban, 404,27 ms aday. Süre oranı 1,274 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/inherited-index-xinfo-disk-verify/res/gates-validation.txt).
