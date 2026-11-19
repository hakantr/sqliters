# V6b.125 — Çoklu WAL→PERSIST/TRUNCATE

Nitelemesiz WAL çıkışında PERSIST ve TRUNCATE hedefleri mevcut ortak commit sırasına katılıyor. İlk ROW eski başlığı ve etkin journalı koruyor. Birden çok gerçek katılımcıda sayfa 1 yazımları ortak journalın kaldırılmasından önce tamamlanıyor. Zaten hedef kipteki dosyaların baytları ve inode'ları korunuyor. [C kanıtı](/github/sqliters/target/perf-work/v124-multi-wal-to-retained-independent/sonuc.md)

216 yeni ve 414 önceki akış C ile eş. 120 erken ROW farkı kapandı. 13.392 ana DB SHA karşılaştırması ve 4.662 salt okunur C açılışı doğrulandı. Birleşik DML OFF 390/400 ve ON 400/400. [Bağımsız kaynak ve ham kanıt denetimi](/github/sqliters/target/perf-work/v124-v125-independent-review/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/multi-wal-to-retained-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 560,89 ms taban, 712,45 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/multi-wal-to-retained-disk-verify/res/gates-validation.txt).
