# V6b.181 — İki kökte rowid IN yazımı

Desteklenen iki doğrudan köklü MAIN rowid tablosunda düz pozitif `rowid IN` filtresi, mevcut tek kök liste uygunluğu ve C aday sırası ile çalışır. Yinelenen eşdeğer değerler tek aday olur. İlk yazıdan önce iki kökün fiziksel karşılaştırıcısı aynı etkin işlem görüntüsünde doğrulanır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v179-two-root-rowid-in.e5OaO0an/sonuc.md)

396 native, 396 takip yazıcısı, 792 rollback ve 1.188 C/OFF/ON yeniden açılışı eş. 120 çok seviyeli akışta iki kökün tam sayfa SHA ve DB görüntüsü doğrulandı. Önceki C bütünlük tanıları ve takip yazması farkı kapandı. [Strict manifest](/github/sqliters/target/perf-work/v179-two-root-rowid-in.e5OaO0an/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/two-root-rowid-in-final-release-proof/res/proof.sha256)

Birleşik CLI ayrı 48 profilde 144 C/OFF/ON yazıcı ve C yeniden açılışında aynı çıktı ve final DB SHA'yı verdi. [Birleşik tekrar](/github/sqliters/target/perf-work/v181-combined-two-index.ofb9sO48/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,48 ms taban, 378,69 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-root-rowid-in-disk-verify/res/gates-validation.txt).
