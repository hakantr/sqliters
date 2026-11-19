# V6b.161 — Çoklu DELETE ve WAL geçişinde BUSY sonrası kip

Koordineli DELETE↔WAL geçişinde bir slot normal BUSY ile durduğunda, önce tamamlanan slotların hedef kipi C gibi ilk gerçek okuma görüntüsüne kadar bağlantıda görünür. Fiziksel sayfa 1 geri alınır. Aynı kip okumadan istenirse yeniden yazma başlamaz. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v160-multi-busy-mode-final.vwQjzw1P/sonuc.md)

228 akışta ilk hata, ara kip, tekrar, gerçek okuma ve takip yazısı eş. 456 son DB dosya çifti bayt eş, 684 C salt okunur açılış başarılı. [2.765 strict kayıt](/github/sqliters/target/perf-work/v160-multi-busy-mode-final.vwQjzw1P/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/multi-busy-mode-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 339,75 ms taban, 416,94 ms aday. Süre oranı 1,227 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/multi-busy-mode-disk-verify/res/gates-validation.txt).
