# V6b.75 — D50 parametre bağlama ve SQL source-id

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

| C kararı | Rust davranışı |
|---|---|
| `vdbeapi.c` vdbeUnbind ve bindText | Geçerli hazır parametrenin önceki değeri temizlenir. TEXT/BLOB giriş bayt sayısı varsayılan sınırı aşarsa rc18 ve NULL parametre. Bağlama hatası step'e ertelenmez. |
| `vdbemem.c` sqlite3VdbeMemSetStr ve sqlite3VdbeMemSetText | Sınırın kendisi kabul edilir. TEXT giriş uzunluğu kodlama dönüşümünden önce karşılaştırılır. |
| `tclsqlite.c` dbPrepareAndBind | Tcl SQL değerlendirmesi bağlama TooBig hatası sonrası NULL ile devam eder. Doğrudan Rust çağrısı hata döndürür. |
| `func.c` sourceidFunc | SQL `sqlite_source_id()` ve sıfır argüman yıldız çağrısı C ile aynı source-id metnini döndürür. Rust public sabiti ürün son ekini korur. |

D50 için mevcut FTS5 testi iki kipte geçti. Küçük sınır deneyinde üç kodlama, dört Rust bağlama girişi ve iki kipte 144 olay ile 24 fiziksel DB bütünüyle eş. C salt okunur yeniden açılışında aynı veri türleri ve `integrity_check=ok`. Rust küçük deney derlemesinde yalnız sınır eşiği C ile aynı test değerine çekildi. Normal çalışma alanı testi varsayılan Rust sabitiyle çalıştı. [D50 kanıtı ve kaynak kararları](/github/sqliters/target/perf-work/bind-size-proof/sonuc.md).

468 native scalar karşılaştırmada 12 source-id vakası iki kipte C'ye hizalandı. Tam API eşliği OFF 396/468 ve ON 420/468. Dosya özetleri önceki koşuyla aynı. Üç kodlama, üç yazma biçimi ve iki işlem kipinde 18/18 C API sonucu, DB SHA ve C yeniden açılış bütünlüğü eş. Group46/65 odak testi iki kipte geçti. [Source-id C/Rust kanıtı](/github/sqliters/target/perf-work/sql-source-id-proof/sonuc.md).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1181 geçti, 1 başarısız, 337 yok sayıldı. ON 1170 geçti, 12 başarısız, 337 yok sayıldı. D50 ve Group46/65 iki kipte başarılı. |
| DML | OFF 380/400, ON 400/400. Ham özetler V6b.74 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet V6b.74 ile aynı. |
| Performans tabanı | 300,85 ms taban, 377,38 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/bind-sourceid-disk-verify/res/gates-validation.txt).
