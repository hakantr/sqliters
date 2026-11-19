# V6b.164 — PERSIST, TRUNCATE ve MEMORY geçişinde BUSY sonrası kip

Koordineli normal WAL giriş ve çıkışında BUSY'ye kadar tamamlanan PERSIST, TRUNCATE ve MEMORY slotlarının hedef kipi C gibi ilk gerçek okumaya kadar görünür. Bu okuma sırasında WAL dosyası ve ana dosya başlığı fiziksel kipi yeniden seçer. Başlık ön okuması bilinçli aynı kip isteğini yeni bir disk yazısına dönüştürmez. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v161-multi-busy-rollback-kinds-corrected.RMz6rvEt/sonuc.md)

576 akışta ilk tanı, geçici kip, takip işlemi ve tekrar eş. 1.152 son ana DB slot çifti bayt eş ve 1.728 C salt okunur açılış geçti. [7.421 strict kayıt](/github/sqliters/target/perf-work/v161-multi-busy-rollback-kinds-corrected.RMz6rvEt/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/multi-busy-rollback-kinds-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,13 ms taban, 372,59 ms aday. Süre oranı 1,246 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/multi-busy-rollback-kinds-disk-verify/res/gates-validation.txt).
