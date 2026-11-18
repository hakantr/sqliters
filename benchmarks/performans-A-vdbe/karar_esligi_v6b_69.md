# V6b.69 — CHECK için kayıt ve varsayılan BLOB kodlaması

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Başlangıç V6b.68 `5bda1defae3bac0f5a436d9bf83939cf5fceae71`.

| C kararı | C sonucu | Rust karşılığı |
|---|---|---|
| `insert.c:2057–2093` | CHECK, yazılacak satırı SQLite boolean dönüşümüyle deniyor. Yanlış değer çatışma kipine göre karar veriyor. NULL geçiyor. | Legacy INSERT ve UPDATE, CHECK sonucunda mevcut kodlamalı boolean dönüşümü kullanıyor. |
| `pragma.c:2039–2055`, `vdbe.c:3336–3342` | Integrity ve quick_check için fiziksel BLOB okunduğunda hücrenin kodlaması veritabanı kodlamasından geliyor. | Legacy integrity check, doğrudan BLOB sütununun fiziksel slotunu kayıt başlığından doğrulayıp o DB'nin kodlamasını kullanıyor. |
| `vdbe.c:3237–3245`, `vdbemem.c:1310–1312` | Fiziksel slot eksik ve literal BLOB varsayılanı varsa `OP_Column P4_MEM` UTF-8 değerini kopyalıyor. | Tam ölçülmüş literal BLOB varsayılanı UTF-8 ile değerlendiriliyor. Rowid alias ve VIRTUAL kaynağı ayrı sınıflanıyor. |

Üç kodlamada her motor 75 native adım işledi. SQL, kabul/ret, extended 275, NULL, OR IGNORE, integrity, quick_check ve son DB özetleri eşit. Altı geçerli C üretimli kısa kayıt iki Rust kipinde `ok` verdi. Önceki deneme, UTF-16BE kısa kaydın geçersiz literal BLOB varsayılanına yanlış `ok` vermişti. Aynı DB'nin SHA-256 değeriyle yeni aday ve C iki denetimde CHECK ihlalini döndürdü.

Yedi default dosyasında sonraki normal INSERT dört kabul ve üç extended 275 sonucu verdi. SQL ve ham DB özetleri eşit. VIRTUAL öncüllü rowid ve PK-first WITHOUT ROWID düzenlerinin iki özel denetimi eşit. Her iki kalıcı regresyon iki kipte geçti. [C dal kapsamı](karar_esligi_v6b_69_dallar.md) · [Açık farklar](gorulen_farklar_v6b_69.md).

Ham kaynak ve ikili manifestleri, fiziksel kayıtlar ve negatif kontrol `/home/hakan/github/sqliters/target/perf-work/check-blob-origin-proof/res` altında. İlk denemenin ayrı negatif kontrolü `/home/hakan/github/sqliters/target/perf-work/check-blob-independent/res/sonuc.md` içinde.

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1169 geçti, 6 başarısız, 337 yok sayıldı. ON 1156 geçti, 19 başarısız, 337 yok sayıldı. İki yeni test iki kipte geçti. Başarısız test kümeleri aynı. |
| DML ve ayrışma bulucu | OFF 380/400, ON 400/400. Finder 245 vaka, stdout 245/245, stderr 244/245. Ham özetler V6b.68 ile eşit. |
| Kurtarma | 147/147, dosya imajı 12/12, gerçek C çöküşü 4/4. |
| Performans tabanı | 301,13 ms taban, 376,19 ms aday. Süre oranı 1,249, 14 digest eşit. |

[Kapı doğrulaması](/home/hakan/github/sqliters/target/perf-work/check-blob-verify/res/gates-validation.txt).
