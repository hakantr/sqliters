# V6b.194 — WAL geri almada sıfır canlı frame

C `walCleanupHash`, paylaşılan WAL indeksinde canlı frame sayısı sıfırsa geri alma sırasında erişilemeyen kayıtları yerinde bırakıyor. Rust aynı koşulu uyguluyor. Sonraki yazıcının ilk frame eklemesi sayfa ve hash bölgesini temizliyor. [C kaynak kararı](/github/sqliters/target/perf-work/v193-restart-spill-savepoint-independent/c/sqlite3.c:68143) · [Bağımsız doğrulama](/github/sqliters/target/perf-work/v193-restart-spill-savepoint-independent/sonuc.md)

432 doğal spill akışı, 1.728 C salt okunur açılışı ve 45 ayrı bağlantı yaşamı eş. Önceki 288 Rust çiftinin SHM farkı kapandı. Cache içinde kalan 432 akış ve V91/V97 korumaları geçti. [Fiziksel denetim](/github/sqliters/target/perf-work/v193-restart-spill-savepoint-independent/res/physical-audit.txt) · [Strict kanıt](/github/sqliters/target/perf-work/v193-restart-spill-savepoint-independent/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 318,33 ms taban, 391,49 ms aday. Süre oranı 1,230 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-zero-frame-undo-disk-verify/res/gates-validation.txt).
