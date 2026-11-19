# V6b.82 — DEFAULT VALUES için tek satırlı FOREIGN KEY kararı

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

C, default değerlerle tek satır yazan INSERT'te uygulanabilir immediate FOREIGN KEY kuralı için önceden `Halt 787` üretir. Rust aynı tek-satır kararını mevcut C uyumlu FOREIGN KEY kontrolüne bağlıyor. Hata öncesinde tablo satırı ve son insert rowid değeri korunuyor. Explicit transaction ve savepoint kapanışında aynı C satırları kalıyor. [C kararı ve bağımsız doğrulama](/github/sqliters/target/perf-work/v82-default-fk-independent/sonuc.md).

Üç kodlama, V1/V2 ve reset/finalize biçimlerinde her Rust kipinde 216/216 işlem durumu, sayaç, satır, DB SHA ve C salt okunur yeniden açılış eş. 36 işlem sınırı ölçümü hatalı INSERT sonrasındaki başarılı COMMIT veya RELEASE ile child verisinin değişmediğini, önceki rowid değerini ve temiz FK/bütünlük kontrolünü doğruladı. 648 fiziksel dosyanın FK kontrolü temiz. Önceki 48 CLI vakası iki kipte 48/48. [Rapor ve kapsam](/github/sqliters/target/perf-work/v82-default-fk-independent/sonuc.md).

V2 ham akışı iki kipte 108/108 eş. V1 ham akışı 48/108. Kalan V1 step tanısı ve açık sütunlu RETURNING'in altı reset/finalize gözlemi ayrı. [Açık API kararları](/github/sqliters/target/perf-work/v82-default-fk-independent/sonuc.md).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 383/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,66 ms taban, 379,56 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/default-fk-disk-verify/res/gates-validation.txt).
