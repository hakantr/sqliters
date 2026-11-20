# V6b.237 — Tarihli ISO 24:00

İşaretsiz ISO tarihli 24:00, canonical saat parçaları ve ham kesir sıfır olduğunda yerel ertesi güne taşınır. Yerel yıl sınırı ofsetten ve son Julian denetiminden ayrı uygulanır. 104 bağlam ve kip başına 1.872 ham kayıt C ile eşleşti. 460 diğer ana bağlam değişmedi. 61 gerçek saklama kontrolü, 923 C açılışı, 12 VDBE testi ve 9.102 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v236-iso-dated-hour24-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,35 ms taban, 381,73 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/iso-dated-hour24-disk-verify/res/gates-validation.txt).
