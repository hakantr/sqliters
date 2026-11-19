# V6b.73 — CHECK ayarının hazırlama snapshot'ı ve TEMP tanısı

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

| C kararı | Beklenen davranış |
|---|---|
| `pragma.c:1158–1181` | `ignore_check_constraints` hazırlanırken bağlantı bayrağı güncelleniyor. Setter çalıştırılırsa statement expiration kararı işletiliyor. |
| `insert.c:2057–2093` | Normal INSERT ve ilgili sütunu değiştiren UPDATE, hazırlanan programın CHECK kararını kullanıyor. NOT NULL, UNIQUE ve STRICT tür denetimi korunuyor. |
| `pragma.c:2039–2055` | `integrity_check` ve `quick_check`, kendi hazırlanma anındaki CHECK kararını kullanıyor. Diğer fiziksel ve indeks denetimleri çalışıyor. |
| `pragma.c:2749–2768` | DEBUG/TEST C derleminde açılmamış TEMP `closed`. Küçük FILE TEMP ve MEMORY `unknown`. FILE TEMP ilk spill'den sonra `unlocked`. |

Legacy yürütücü mevcut bağlantı bayrağından hazırlamada aldığı değeri ifade ve yeniden hazırlama boyunca taşıyor. Yalnız hazırlanmış PRAGMA setter daha önce hazırlanan ifadeyi değiştirmiyor. Yürütülen setter sonrasında V2 yeni bayrağı alabiliyor. [Bağımsız C/Rust kanıtı](/github/sqliters/target/perf-work/check-pragma-disk-reproof/sonuc.md).

Üç kodlama ve 84 native vakada CHECK ayarı adayı, arşivdeki exact tabanla tekrar ölçüldü. Tam API OFF 63/84 ve ON 65/84. Son DB SHA OFF 78/84 ve ON 80/84. İlave 12 kısıt korumasında 12/12 DB eşliği. Kalan üç API farkı eski V1 step maskelemesi. V6b.69'un iki BLOB CHECK testi iki kipte geçti. [Ham sayım](/github/sqliters/target/perf-work/check-pragma-disk-reproof/res/verified.txt) · [Kaynak özeti](/github/sqliters/target/perf-work/check-pragma-disk-reproof/res/full-sources.sha256).

Çoklu DB testindeki küçük FILE TEMP kilit beklentisi ayrıca güncellendi. DEBUG C referansında MEMORY/FILE, boş/küçük/spill ve üç yazma durumu 18 vaka. Her iki Rust kipinde tüm SQL tanı satırları ve her iki kalıcı DB özeti 18/18 eş. Testin kalan bölümleri odak koşusunda iki kipte geçti. [C ve Rust ham sonuçları](/github/sqliters/target/perf-work/temp-lock-status-proof/res/native.json) · [OFF odak](/github/sqliters/target/perf-work/temp-lock-status-proof/res/rust-off.txt) · [ON odak](/github/sqliters/target/perf-work/temp-lock-status-proof/res/rust-on.txt).

Tam V6b.72 üzerine birleşik aday yeniden koşuldu. 84+12 native vaka önceki bağımsız kayıtlarla aynı sonucu verdi. [Birleşik C/Rust ve kaynak kanıtı](/github/sqliters/target/perf-work/check-temp-v73-independent/sonuc.md).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1179 geçti, 3 başarısız, 337 yok sayıldı. ON 1167 geçti, 15 başarısız, 337 yok sayıldı. F12 ve iki yeni CHECK testi iki kipte geçti. |
| DML | OFF 380/400, ON 400/400. Ham özetler V6b.72 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet V6b.72 ile aynı. |
| Performans tabanı | 300,70 ms taban, 376,72 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/check-temp-disk-verify/res/gates-validation.txt).
