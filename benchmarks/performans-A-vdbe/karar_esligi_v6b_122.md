# V6b.122 — İpucusuz açık SELECT sırası

İndeks ipucu bulunmayan doğrulanan SELECT'te gerçek C programının tek `OpenRead` kökü ve KeyInfo değeri, fiziken açılan indeksi belirliyor. Ölçülen collation uyuşmazlığında mevcut ilişkisel sıralama kullanılıyor. [Kaynak ve yama](/github/sqliters/target/perf-work/v120-unhinted-index-order-independent/sonuc.md)

Dört yeni ipucusuz ASC/DESC ve dört önceki açık indeks farkı C eş. 24 Rust ve altı C salt okunur grup, 16 fallback izi ve altı dosya SHA koruması geçti. Birleşik optimize DML 390/400 OFF, 400/400 ON ve önceki ham özetle eş. [Exact bağımsız ölçüm](/github/sqliters/target/perf-work/unhinted-index-order-v122-proof/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/unhinted-index-order-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,71 ms taban, 379,08 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/unhinted-index-order-disk-verify/res/gates-validation.txt).
