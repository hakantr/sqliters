# V6b.102 — Nitelikli korelasyonlu EXISTS ve dış covering tarama

İç indeks eşitliği, açık nitelikli dış sütun bağımlılığı ve indeksin o sütunları sağlaması birlikte doğrulanıyor. Dış covering tarama C'nin gerçek sıra kararını izliyor. Özgün EXISTS yürütmesi ve normal planlayıcı kullanılıyor. [C kaynak ve exact ölçüm](/github/sqliters/target/perf-work/correlated-exists-covering-independent/sonuc.md)

366 native grupta V1/V2 ayrı OFF ham 68/183'ten 141/183'e, ON 106/183'ten 155/183'e çıktı. Fiziksel dosyalar ve C açılışları 183/183. Aynı ve farklı kaynaklı self REPLACE SELECT'te son rowid ve çakışan satır değeri C ile eş. Önceki 79 grubun ham sonuçları korundu. Birleşik DML OFF 390/400 ve ON 400/400, önceki ham özet eş. [Bağımsız SHA](/github/sqliters/target/perf-work/correlated-exists-covering-independent/sha256.txt) · [Tam korpus](/github/sqliters/target/perf-work/correlated-exists-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 564,21 ms taban, 717,83 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/correlated-exists-disk-verify/res/gates-validation.txt).
