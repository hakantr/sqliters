# V6b.80 — V1 okuma kesintisinde bağlantı tanısı

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

| C kararı | Rust davranışı |
|---|---|
| `sqlite3Step` ve `sqlite3_errmsg` | Varsayılan V1 okuma kesintisi step çağrısında rc1 ile bağlama ait saklı mesajı ve bayt ofsetini kullanır. |
| `sqlite3VdbeReset` ve `sqlite3VdbeTransferError` | Çalışmış okuma ifadesinin reset/finalize çağrısı gerçek rc9'u verir ve uygun durumda bağlantıya aktarır. |
| `sqlite3Prepare` | Başarılı hazırlama saklı tanıyı temizler. Hatalı hazırlama mesajı ve ilgili ofseti taşır. |
| `vdbeUnbind` | Positional bind range hatası saklı mesajı temizler. C'nin sakladığı önceki ofset ayrı değerlendirilir. |

240 yaşam, 54 peer ve 12 gerçek aktarım vakası toplam 306 native C/Rust karşılaştırması. Her iki Rust kipinde 306/306 olaylar, fiziksel DB SHA ve C yeniden açılışı eş. Önceki eş olaylarda gerileme yok. Group3 dahil yedi odak iki kipte başarılı. Harici AtomicBool kullanan 12 vakada tabanın ham sonucu ve DB SHA değeri korundu. [Bağımsız V1 kanıtı](/github/sqliters/target/perf-work/v1-retained-independent/sonuc.md).

Kesin V79 üzerine tekrar ölçülen aday 324/324 native vakada iki kipte C eş. Önceki 306 vaka ham olaylar, DB SHA ve yeniden açılışta birebir. 18 ek olay V77 skaler ve view tanısı, başarılı hazırlama ile temizleme ve V79 truth peer kullanımı sonrasında aynı saklı mesaj/ofset aktarımını gösteriyor. [Exact V80 raporu](/github/sqliters/target/perf-work/v80-v1-independent/sonuc.md) · [Kaynak ve ham kanıt manifesti](/github/sqliters/target/perf-work/v80-v1-independent/res/proof.sha256).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni V1 kesinti testi iki kipte başarılı. |
| DML | OFF 382/400, ON 400/400. Ham özetler V6b.79 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özetler V6b.79 ile aynı. |
| Performans tabanı | 297,98 ms taban, 373,58 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/v1-retained-disk-verify/res/gates-validation.txt).
