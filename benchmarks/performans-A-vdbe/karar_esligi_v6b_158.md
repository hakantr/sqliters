# V6b.158 — Retained WAL takip yazısında rollback başlığı

Mantıksal rollback kipine geçen bağlantıda fiziksel WAL oturumu açıkken, mevcut (1,1) sayfa 1 formatı C gibi korunur. Temiz sayfa 1 yalnız kip baytları nedeniyle frame listesine alınmaz. Gerçek sayfa değişimi, dosya büyümesi ve commit çerçevesi mevcut WAL kararını izler. [C kaynak sırası ve yama](/github/sqliters/target/perf-work/v155-retained-rollback-header-independent/sonuc.md)

96 native retained akışta 64 eski sayfa biçimi farkı kapandı. 96 takip UPDATE commit'i yalnız değişen veri sayfası frame'ini yazdı. 126 koruma, 2.036 C/Rust DB SHA ve 1.008 C yeniden açılışı eş. [12.952 strict kayıt](/github/sqliters/target/perf-work/v155-retained-rollback-header-independent/res/proof.sha256) · [Birleşik kanıt](/github/sqliters/target/perf-work/retained-rollback-header-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 314,79 ms taban, 385,59 ms aday. Süre oranı 1,225 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/retained-rollback-header-disk-verify/res/gates-validation.txt).
