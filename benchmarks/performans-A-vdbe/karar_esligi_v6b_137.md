# V6b.137 — İzinli WAL→OFF statement bitişi

Normal ve ilgili ayarın izinli olduğu NamedDisk bağlantısında WAL checkpoint ve yan dosya sonlandırması ilk ROW öncesinde tamamlanır. Sayfa 1 (2,2) kalır, DONE/reset/finalize (1,1) yazımını C ile aynı boyut ve ofsette bitirir. Gerçek OFF katılımcısı fiziksel rollback journal açmaz. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v136-wal-to-off-independent/sonuc.md)

864 izinli, 378 önceki kip ve 270 Defensive akışı C ile eş. 504 önceki sınır farkı kapandı. 28.080 ana dosya SHA, 10.530 C salt okunur açılışı, 3.024 ham syscall ve 29 izole C kapsam profili doğrulandı. [Strict fiziksel kanıt](/github/sqliters/target/perf-work/v136-wal-to-off-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/wal-to-off-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,45 ms taban, 377,71 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-to-off-disk-verify/res/gates-validation.txt).
