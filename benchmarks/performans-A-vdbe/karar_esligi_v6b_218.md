# V6b.218 — Aynı tablo kaynaklı INSERT SELECT aralığı

Ölçülen bağımsız sayısal scalar alt sınırı mevcut örtücü indeks aralığında kullanılıyor. 48 OFF yazım profili C ile eşleşti. Yazım hook sırası, UNIQUE hata sonucu, geri alınan veri ve son rowid doğrulandı. 2.075 profil ve 8.018 C salt okunur açılışı ölçüldü. [Bağımsız kanıt](/github/sqliters/target/perf-work/v218-same-table-scalar-range-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 314,87 ms taban, 416,61 ms aday. Süre oranı 1,323 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/same-table-insert-scalar-range-disk-verify/res/gates-validation.txt).
