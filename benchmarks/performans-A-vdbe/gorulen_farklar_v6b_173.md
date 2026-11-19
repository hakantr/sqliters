# V6b.173 — Açık kapsam

Soğuk setter prepare zamanlaması ve varsayılan salt okunur terminal VFS hazırlığı açık. Linux native ölçüldü. Native Windows ve diğer Unix platformları için bu aşamada yerel koşu yok. [Kapsam raporu](/github/sqliters/target/perf-work/v171-readonly-wal-exit-lock-independent/sonuc.md)

Expired ikinci V1 step ve finalize dönüş kodları ölçüldü. Bu çağrılardan sonraki bağlantı hata kodu ve mesajını koruma ayrıntısı açık.

Suite8 tam native Unix VFS için C’nin salt okunur WAL çıkışında gözlenen 3850 kodunu bekliyor. Native yeteneği bulunmayan platformlarda READONLY beklentisi korunuyor. İlk kapı eski READONLY beklentisinde durdu. [İlk OFF kanıtı](/github/sqliters/target/perf-work/v173-suite8-expectation.cpaNPdf9/first-full-gate/res/tests_off.txt) · [Pinli C kanıtı](/github/sqliters/target/perf-work/v171-readonly-wal-exit-lock-independent/sonuc.md)
