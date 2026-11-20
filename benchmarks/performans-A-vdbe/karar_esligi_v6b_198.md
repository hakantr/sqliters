# V6b.198 — max_page_count değer dönüşümü

Codegen ve yürütme, SQLite 3.54.1 ile aynı başarılı tamsayı dönüşümünü, sıfır sorgusunu ve en yüksek sayfa sınırını kullanıyor. Ham sayısal tokenın yazımı bu PRAGMA için korunuyor. Bir veritabanının pozitif limiti mevcut sayfa sayısının altına düşmüyor. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v197-max-page-count-independent/sonuc.md)

12 profil, 36 akış, 4.608 hazırlanmış sorgu ve 72 salt okunur C yeniden açılışı eşleşti. Ana, bağlı ve TEMP limitlerinin bağımsızlığı, sınır altındaki BLOB yazımları, önceki satırlar ve DB dosya özetleri doğrulandı.

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 297,88 ms taban, 373,54 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/max-page-count-value-disk-verify/res/gates-validation.txt).
