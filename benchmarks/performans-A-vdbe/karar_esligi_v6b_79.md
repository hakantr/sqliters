# V6b.79 — TRUE ve FALSE ad çözümü ve IS işlemi

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

C, gölgeleme bulunmayan `IS TRUE` ve `IS FALSE` sağ işlenenlerini boolean test olarak çözüyor. Olağan `IS` sayısal eşitliği kendi kurallarını koruyor. NULL için iki pozitif truth testi false, tersleri true. Kollasyon ve parantez, C'nin çözümlenebilen truth biçimlerinde aynı ayrımı izliyor. TRUE ve FALSE, C'nin izin verdiği identifier ve alias konumlarında ad olarak ayrıştırılıyor. Üretim çözüm yolu kaynak sütun adlarını bildiği sorgu kapsamlarını ele alıyor.

480 değer iki kipte C ile tam eş. 72 UPDATE ve DELETE vakası üç kodlama ile rowid, UNIQUE ve WITHOUT ROWID anahtarları içeriyor. Her iki Rust kipinde 72 DB SHA eş. 648 çapraz yeniden açılış C ile eş ve `integrity_check=ok`. Sanal tablo odaklarında constraint ve xFilter argümanları C eş. C ve Rust arasında önceki xBestIndex çağrı sayısı farkı sürüyor. [Bağımsız C/Rust kıyaslaması](/github/sqliters/target/perf-work/is-truth-native-independent/sonuc.md).

Normal `IS 1` ve `=1` tek anahtar yolunu, `IS TRUE` tüm sayısal true anahtarları koruyor. Kısmi ve ifade indeksleri ile belirsiz alias ve join kapsamı ayrı. [Truth DML ham kayıtları](/github/sqliters/target/perf-work/is-truth-native-independent/res/dml/cases.json).

Kesin V76 ve V77/V78 kaynaklarına uygulanan birleşik aday, önceki 480 değer, 72 DML, 648 yeniden açılış ve 60 indeks kontrolünü yeni ikiliyle tekrar doğruladı. 18 ek kesişim vakasında C hata ofseti, generated indeks yolu, SQL satırları, dosya SHA ve C yeniden açılışı eş. Parser 11/11 ve üç runtime testi her iki kipte geçti. [Kesin aday ve kaynak manifesti](/github/sqliters/target/perf-work/v79-truth-independent/sonuc.md).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1185 geçti, 1 başarısız, 337 yok sayıldı. ON 1176 geçti, 10 başarısız, 337 yok sayıldı. Dört yeni test iki kipte geçti. |
| DML | OFF 382/400, ON 400/400. Ham özetler V6b.78 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet V6b.78 ile aynı. |
| Performans tabanı | 318,74 ms taban, 397,43 ms aday. Süre oranı 1,247 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/truth-comparison-disk-verify/res/gates-validation.txt).
