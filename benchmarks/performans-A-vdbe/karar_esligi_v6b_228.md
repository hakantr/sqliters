# V6b.228 — Backup ilk sayfa edinmesi

İlk kopyada kaynak rollback pager sayfasının edinme ömrü C ile eş. 90 hedef dosya farkı kapandı. 840 normal ve 144 geniş profil doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v226-backup-first-copy-acquire-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,11 ms taban, 377,07 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/backup-first-copy-acquire-disk-verify/res/gates-validation.txt).
