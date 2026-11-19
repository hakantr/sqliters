# V6b.96 — Görünüm DELETE ve INSTEAD OF trigger sırası

Doğrudan projeksiyonlu basit ana şema görünümünde, yan etkisiz sayısal aralık C gibi geçici görünüm SELECT'ine taşınıyor. Normal erişim planı bu aralığa uygun tek BINARY indeksi seçebiliyor. Dış DELETE filtresi doğrulanmaya devam ediyor. C geçici rowid sırası, INSTEAD OF DELETE çağrı sırasına geçiyor. [Bağımsız kaynak ve dosya kararı](/github/sqliters/target/perf-work/view-delete-order-independent/sonuc.md)

282 taban ve 282 aday grubu üç kodlama ve iki hazırlama API'siyle ölçüldü. Ham API, fiziksel dosya ve C açılışı OFF V1/V2 ayrı 42/141'den 111/141'e ve ON 75/141'den 121/141'e çıktı. Yeni fark ve değişen açık sonuç yok. Ana tablonun son satırları tüm gruplarda C eş. Önkoşu OFF 387/400'den 388/400'e çıktı. 6045 trigger izi eşitlendi. ON 400/400 ve önceki ham sonuçlar aynı. [Bağımsız kanıt](/github/sqliters/target/perf-work/view-delete-order-independent/sha256.txt) · [Tam korpus](/github/sqliters/target/perf-work/view-delete-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 388/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,59 ms taban, 377,52 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-delete-disk-verify/res/gates-validation.txt).
