# V6b.87 — TRUE ve FALSE adlı gerçek sütunlar

Bilinen tek normal tabloda hazırlama, C ile aynı biçimde gerçek sütunu WHERE takma adı ve boolean literal öncesinde çözüyor. Önceki kapsamla birleşince SELECT projeksiyonu ve WHERE ile olağan tek tablo UPDATE/DELETE WHERE akışı gerçek sütunu buluyor. `IS` için gerçek sütun ve boolean literal ayrımı korunuyor. [Bağımsız kaynak ve kıyas](/github/sqliters/target/perf-work/v85-truth-column-independent/sonuc.md)

Her kipte 114 hedef akış C ile ham eş. 24 yazma akışının dosya SHA-256 değeri ve C açılışı eş. 36 koruma ve önceki 297 oturumda gerileme çıkmadı. Her kipte 33 önceki tanı sonucu korundu. [Kanıt manifesti](/github/sqliters/target/perf-work/v85-truth-column-independent/res/proof.sha256)

Birleşik güncel kaynaklarda odak yeniden doğrulandı. 114 hedef ve 36 koruma, 750 C açılışı. [Kesin taban kanıtı](/github/sqliters/target/perf-work/truth-exact-native-focus/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,24 ms taban, 379,82 ms aday. Süre oranı 1,261 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/truth-column-disk-verify/res/gates-validation.txt).
