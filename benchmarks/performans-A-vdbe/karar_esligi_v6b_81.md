# V6b.81 — WITHOUT ROWID STORED tam tarama kararı

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

C aynı ölçülen maliyette pIndex zincirinde önce gelen döngüyü tutuyor. STORED üretilen sütunlu WITHOUT ROWID tablonun sırasız tam taramasında PK ve ikincil indeksin fiziksel kolon boyutu ayrı hesaplanıyor. Aday koşulu sıralama, predicate, indeks ipucu ve ANALYZE bilgisi olmayan kapsamı ele alıyor. [C maliyet ve sıra kanıtı](/github/sqliters/target/perf-work/without-rowid-scan-independent/sonuc.md).

Geniş adayın tam release önkoşusu bir normal INSERT SELECT sıra farkı gösterdi. Son aday STORED üretilen sütunlu tablolarla sınırlandı. Aynı örneğin C, V80 ve son aday ham gözlemleri, yeniden açılışı ve fiziksel DB SHA değerleri iki kipte eş. Uygulanan dar aday ile tam release DML 382/400 ve 400/400, V80 önceki ham DML kaydıyla aynı. [Dar aday ve ayrıntılı kanıt](/github/sqliters/target/perf-work/without-rowid-stored-scope/sonuc.md) · [Tam release manifesti](/github/sqliters/target/perf-work/without-rowid-stored-release-proof/res/proof.sha256).

712 normal grup adayın iki kipinde C yeniden açılış ve fiziksel SHA eşliği verdi. Önceki 415 grubun ham sonuçları iki kipte C ile aynı. Ek 18 kesişim ve 48 INSERT/REPLACE iki kipte tam C eş. Diğer 279 gruptaki 144 SELECT'te tam ham eşlik OFF 120/144, ON 128/144. Geniş adayın 42 farklı SELECT sıra gözlemi V80'e döndü ve fiziksel dosya eşliği korundu. [Kesin kayıt manifesti](/github/sqliters/target/perf-work/without-rowid-stored-scope/res/proof.sha256).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 382/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 558,28 ms taban, 553,78 ms aday. Süre oranı 0,992 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/without-rowid-stored-disk-verify/res/gates-validation.txt).
