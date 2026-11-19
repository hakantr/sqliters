# V6b.149 — WAL→MEMORY çıkışında PERSIST_WAL

Normal tek veya nitelikli WAL→MEMORY geçişi mevcut C PERSIST_WAL açma ve sonlu boyut kararını kullanır. Varsayılan sınırda retained WAL yeniden açılır, sonlu sıfır dosya yerleşik VFS kuralıyla ayrılır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v145-wal-memory-persist-independent/sonuc.md)

708 doğrudan ve 738 koruma akışı doğrulandı. 144 fiziksel ve 288 ilk ROW VFS bildirimi farkı giderildi. 132 canlı C salt okunur eş bağlantı, 4.720 ana DB SHA çifti, 2.360 C açılış çifti ve 38 izole C profil geçti. [53.353 strict kayıt](/github/sqliters/target/perf-work/v145-wal-memory-persist-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/wal-memory-persist-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,61 ms taban, 376,69 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-memory-persist-disk-verify/res/gates-validation.txt).
