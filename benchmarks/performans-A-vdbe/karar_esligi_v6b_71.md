# V6b.71 — Skaler fonksiyonda yıldız ve sıfır argüman

Referans C SQLite 3.54.1 kaynak kimliği `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

C `parse.y:1248–1250` fonksiyon çağrısında yıldız için boş argüman listesi oluşturuyor. `resolve.c:1151–1175` gerçek argüman sayısını sıfır olarak çözüyor. Legacy Rust skaler fonksiyon doğrulaması aynı ariteyi kullanıyor. Normal argüman listesi ve COUNT gibi ayrı aggregate dalları korunuyor.

39 ifade, üç DB kodlaması, V1/V2 ve iki SELECT biçimiyle 468 native vaka çalıştırıldı. Yıldız ve boş parantez her vakada birlikte ölçüldü. Tam C eşliği OFF 12/468'den 384/468'e, ON 160/468'den 408/468'e çıktı. DB SHA-256 değerleri 468/468 iki kipte eşit. Boş parantez sonuçları tabanla aynı. Required arity, bulunmayan ad, aggregate ve pencere sınırları yerel kanıtta ayrı kaydedildi. `random` çıktısının türü ölçüldü. Group46/65 C'ye göre güncellenen beklentilerle iki kipte geçti. [Ham aday](/home/hakan/github/sqliters/target/perf-work/scalar-star-parity/res/native/cases.json) · [Tam taban](/home/hakan/github/sqliters/target/perf-work/scalar-star-parity/res/before/cases.json) · [Yeniden sayım](/home/hakan/github/sqliters/target/perf-work/scalar-star-parity/res/verified.txt).

| C kararı | Gcov |
|---|---|
| `parse.y:1248–1250` | Yıldızlı fonksiyon indirgemesi 468, boş argüman listesiyle fonksiyon kurulum çağrısı 468. |
| `resolve.c:1151–1175` | 1128 çözümleme. 984 ilk eşleşme. İlk aramada eşleşmeyen 144. Sonraki aramada 24 bulunmayan ad ve 120 arite hatası. |

Native matris doğrudan SELECT ve CTE kullanıyor. UDF arite gölgeleme, authorizer reddi, index veya üretilen sütunda nondeterministic koruma, schema reprepare ve bellek tahsis hatası bu kapsamda ayrı çalıştırılmadı. [C kapsam çıktısı](/home/hakan/github/sqliters/target/perf-work/scalar-star-parity/res/native/gcov/sqlite3.c.gcov) · [Kaynak ve ham sonuç SHA-256](/home/hakan/github/sqliters/target/perf-work/scalar-star-parity/res/verified-sha256.txt).

Legacy V1/V2 hata ofseti farkları açık. OFF üç FILTER/OVER/arity kontrolünde C mesajını ve kodunu döndürdü, ofset -1 kaldı. ON altı ek kontrolde eşit. `sqlite_source_id(*)`, kabul sonrası `sqlite_source_id()` ile aynı SQLiters son ekini gösteriyor. C kimliğinde bu son ek bulunmuyor. [Altı guard ham izi](/home/hakan/github/sqliters/target/perf-work/scalar-star-parity/res/guards/cases.json).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1170 geçti, 5 başarısız, 337 yok sayıldı. ON 1158 geçti, 17 başarısız, 337 yok sayıldı. Group46/65 ON başarısızlığı giderildi. Yeni başarısızlık yok. |
| DML | OFF 380/400, ON 400/400. Ham özetler V6b.70 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet V6b.70 ile aynı. |
| Performans tabanı | 298,37 ms taban, 370,14 ms aday. Süre oranı 1,241 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. İlk disk derlemesi G/Ç beklemesinde tamamlanamadı. Geçici bellek kopyası C fixture'larının beklediği Git/dizin yerleşimini sağlamadı. Bu iki koşu kabul hesabına alınmadı. Dört kapı, taşınan gerçek çalışma ağacında aynı kaynak özetleriyle tamamlandı. [Son kapı doğrulaması](/home/hakan/github/sqliters/target/perf-work/scalar-star-disk-verify/res/gates-validation.txt).
