# V6b.196 — WAL checkpoint kip adı

`wal_checkpoint` kipinde C'nin tam ad kararı uygulanıyor. Tanınmayan metin PASSIVE oluyor. `'truncate '` ve `'noop '` değerlerinin önceki Rust davranışı 96 karşılaştırmada gözlenen sonucu veya dosya evresini değiştirmişti. Adayda 360 C–Rust SQL ve yeniden açılış çifti eş. [C kaynak kararı](/github/sqliters/target/perf-work/v194-detach-wal-close-independent/c/sqlite3.c:147714) · [Native karşılaştırma](/github/sqliters/target/perf-work/v195-checkpoint-mode-spelling.CGOe0wLD/sonuc.md)

1.728 ham fiziksel evre eş. Kanonik TRUNCATE grubunun 72 evresinde salt ve bağlı SHM checksum doğrulandı. [Fiziksel denetim](/github/sqliters/target/perf-work/v195-checkpoint-mode-spelling.CGOe0wLD/res/audit.txt) · [Strict kayıt](/github/sqliters/target/perf-work/v195-checkpoint-mode-spelling.CGOe0wLD/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 307,15 ms taban, 440,61 ms aday. Süre oranı 1,435 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/checkpoint-mode-spelling-disk-verify/res/gates-validation.txt).
