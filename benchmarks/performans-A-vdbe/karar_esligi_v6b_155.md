# V6b.155 — Normal OFF ve MEMORY commit phase-two

Normal disk OFF ve MEMORY commit'i, C gibi mevcut finalize işleminden sonra COMMIT_PHASETWO bildirir ve SHARED kilidine iner. Geçişe ait phase-two yolunda kullanılan dosya ve kilit koşulları ordinary commit için de uygulanır. [C kararı ve bağımsız yama](/github/sqliters/target/perf-work/v153-ordinary-rollback-phase-two-independent/sonuc.md)

612 sağlıklı akışta 948 callback çifti hizalandı. Önceki 132 OFF çifti tekrarlandı. 6.654 ana DB SHA, 3.168 C salt okunur açılış ve 540 takip işlemi doğrulandı. [26.401 strict kayıt](/github/sqliters/target/perf-work/v153-ordinary-rollback-phase-two-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/ordinary-rollback-phase-two-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,41 ms taban, 375,65 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/ordinary-rollback-phase-two-disk-verify/res/gates-validation.txt).
