# V6b.200 — DDL rollback ve V2 okuyucu ömrü

Eski Btree’de gerçek okuma yapan ROW duruyor. C’nin tamamlanmış sorter çıktısı kalan satırları ve DONE sonucunu veriyor. OFF sıraya koyma kararı ve Group5’in sorter beklentisi bu ayrımı izliyor. Olağan DML rollback devam ediyor. [Bağımsız karar](/github/sqliters/target/perf-work/v199-rollback-cursor-v2-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,68 ms taban, 374,82 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/rollback-sorter-cursor-v2-disk-verify/res/gates-validation.txt).
