# V6b.130 — Bağlantı kapanışında disk sırası

Toplu Btree kapanışı MAIN, TEMP ve ATTACH slotlarını C sırasıyla kapatır. Normal close, boş bağlantının Drop akışı ve yaşayan son statement ile tamamlanan deferred close aynı sıralamayı kullanır. 216 diskler arası sıra farkı düzeldi. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v128-close-slot-order-independent/sonuc.md)

846 sağlıklı akışta 9.360 ana DB SHA, 2.376 C salt okunur açılışı, 3.108 gerçek ON yürütme izi ve 14 izole C gcov profili doğrulandı. Ham WAL frame payload'ları kapanışta yazılan ana dosya görüntülerini oluşturuyor. [Ham fiziksel denetim](/github/sqliters/target/perf-work/v128-close-slot-order-independent/res/physical-audit.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/close-slot-order-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,43 ms taban, 379,34 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/close-slot-order-disk-verify/res/gates-validation.txt).
