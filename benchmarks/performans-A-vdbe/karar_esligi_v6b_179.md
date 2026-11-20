# V6b.179 — İki kökte sıralı INSERT SELECT

Desteklenen iki doğrudan indekse sahip MAIN rowid hedefi, ayrı MAIN rowid tablosundan açık hedef listesi ve rowid ASC/DESC sırasıyla INSERT SELECT çalıştırır. Hedefte ilk yazıdan önce iki fiziksel kök aynı etkin yazma görüntüsünde doğrulanır. C'nin kaynak ziyaret yönü ve son oluşturulan indeks önce yazma sırası korunur. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v177-two-root-insert-select.TZM0UxYO/sonuc.md)

396 native yazıcı, 396 takip yazıcısı, 792 rollback ve 1.188 yeniden açılış eş. Çok sayfalı 192 akışta 96 C–Rust ve 24 eski iç OFF–ON dosya çifti eş. C'nin önceki 20 karma indeks bütünlük tanısı kapandı, takip yazması başarısı ve kaynak sayfalarının SHA eşliği doğrulandı. [Strict manifest](/github/sqliters/target/perf-work/v177-two-root-insert-select.TZM0UxYO/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/two-root-insert-select-final-release-proof/res/proof.sha256)

Birleşik CLI 48 taze 196 hedef/96 kaynak satırlı profil ve 144 yazıcıda, C yeniden açılışıyla birlikte ham çıktıyı ve final DB SHA'yı eşledi. [Birleşik tekrar](/github/sqliters/target/perf-work/v179-combined-two-index.dQipbAwO/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 307,91 ms taban, 388,58 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-root-insert-select-disk-verify/res/gates-validation.txt).
