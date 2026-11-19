# V6b.86 — INDEXED BY ve kalıcı şema indeksleri

`INDEXED BY` ad çözümlemesinde aynı tabloya ait kalıcı PRIMARY KEY ve UNIQUE indeksleri C ile aynı biçimde bulunuyor. Şema sahipliği, kısmi indeksin kullanılabilirliği ve bir rowid alias için fiziksel indeks bulunup bulunmadığı denetleniyor. SELECT, INSERT SELECT, UPDATE ve DELETE için ortak çözüm kullanılıyor. [C kaynak ve bağımsız kapsamı](/github/sqliters/target/perf-work/named-schema-index/sonuc.md)

V1 ve V2 ayrı ayrı OFF ham eşlik 27/93'ten 93/93'e, dosya ve C açılışı 75/93'ten 93/93'e çıktı. Doğru ON'da ham eşlik 49/93'ten, fiziksel eşlik 81/93'ten 93/93'e çıktı. İlk ON koşusunun ortam anahtarı yanlıştı. Düzeltilen koşuda UTF-8 SELECT ve INSERT SELECT makinede ve UTF-16LE SELECT fallback'te ölçüldü. [Düzeltilmiş ON kanıtı](/github/sqliters/target/perf-work/named-schema-index/on-duzeltme.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,37 ms taban, 379,56 ms aday. Süre oranı 1,251 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/named-index-disk-verify/res/gates-validation.txt).
