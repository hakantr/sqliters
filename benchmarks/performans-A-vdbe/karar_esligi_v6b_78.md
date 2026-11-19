# V6b.78 — Üretilen sütunlar için C tarama kararı

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

C üretilen sütuna başvurulduğunda tüm tablo sütunlarını kullanım maskesine alır. C'nin explicit tam indeks, fiziksel sütun maskesi ve uygun ifade indeksinde alternatif yürüyüş kararları Rust planlayıcısına taşındı. VIRTUAL sütunların maske kapsamı, STORED sütunlardan farklı. İndekslenen tam ifade bulunduğunda alt ifadelerin sütunları yeniden denetlenmez.

Bu sıra kalıcı veriyi etkiliyor. Aynı UNIQUE anahtarına birden çok sırada yazan ordinary REPLACE SELECT akışında legacy taban farklı bir son satır saklıyordu. Adayda iki Rust kipi C ile ham SQL sonucu, salt okunur yeniden açılan tablo ve SHA-256 bakımından aynı. Bu sonuç, çok kodlamalı DML kontrolleriyle ve mevcut korpus akışıyla doğrulandı. [Minimal C sonucu](/github/sqliters/target/perf-work/ordinary-dml-row-diffs/res/replace-select/C.out) · [Bağımsız aday ve dosya kanıtı](/github/sqliters/target/perf-work/generated-scan-independent/sonuc.md).

415 sorgu grubunda iki tekrar ile 4.150 dosya C tarafından açıldı. Bütünlük sonuçları temiz, yeniden açılan satırlar ve DB SHA değerleri iki kipte 415/415 eş. 96 küçük ve 72 geniş sütunlu vakada ölçülen ham SQL sonuçları iki kipte eş. 180 layout kontrolünde OFF 177/180 ve ON 178/180. Eski eş sonuçlarda gerileme yok. [Ham sonuçlar](/github/sqliters/target/perf-work/generated-scan-independent/res/reviewed-matrix/result.json) · [Kaynak ve kanıt manifesti](/github/sqliters/target/perf-work/generated-scan-independent/res/proof.sha256).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1181 geçti, 1 başarısız, 337 yok sayıldı. ON 1172 geçti, 10 başarısız, 337 yok sayıldı. V6b.77'ye göre yeni başarısızlık yok. |
| DML | OFF 382/400, ON 400/400. OFF iki yeni eş vaka. Açık vakaların ilk gözlemi aynı. Tam ham sonuç kesin release önkoşusuyla eş. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet V6b.77 ile aynı. |
| Performans tabanı | 299,83 ms taban, 375,21 ms aday. Süre oranı 1,251 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/generated-scan-disk-verify/res/gates-validation.txt).
