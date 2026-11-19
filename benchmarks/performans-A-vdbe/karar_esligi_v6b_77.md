# V6b.77 — Skaler SELECT ve view UPSERT hazırlama tanıları

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

| C kararı | Rust davranışı |
|---|---|
| `resolve.c` fonksiyon adı ve argüman kontrolü | Legacy SELECT hata kodu ve metni C portuyla aynı olduğunda eksik UTF-8 token ofseti alınır. Sessiz prob callback çalıştırmaz ve hazırlama yan etkilerini yaymaz. |
| `insert.c:983–1005` ve `delete.c:119–134` | Uygun INSERT tetikleyicisi bulunmayan view önce yazılabilirlik tanısını verir. |
| `insert.c:1284–1293` | Uygun tetikleyici varsa kaynak sütun, arite veya ifade tanısı önce gelebilir. Ardından `cannot UPSERT a view`. |
| İlk hazırlama ve yeniden hazırlama | Ortak tanı işlevi yalnız legacy reddi, skaler veya gerçek view UPSERT kapsamına girince çalışır. |

V6b.75 tabanıyla 468 skaler SELECT hazırlığı ve dosya kaydı iki kipte 468/468 C eş. Önceki 396 OFF ve 420 ON vakalarının sonuçları ile tüm rota sayaçları korundu. 234 view hazırlığı ve altı V2 yeniden hazırlama iki kipte hata kodu, mesajı, ofseti, DB SHA ve C yeniden açılışında eş. Tam phase3/phase4 odak dört koşuda başarılı. [Birleşik kanıt](/github/sqliters/target/perf-work/prepare-diagnostics-independent/sonuc.md).

66 ek callback vakasında uygulama işlevi, authorizer ve sanal tablo çağrıları önceki tabanla aynı. Kayıtlı işlev aritesi, UTF-8 önek ve alıntılı ad tanılarında 30/30 C eşliği. Bazı sanal tablo ofsetleri, authorizer reddi ve V1 şema değişimi maskesi açık. [Ek ham kayıtlar](/github/sqliters/target/perf-work/prepare-diagnostics-independent/res/guard-summary.json) · [Kaynak ve kayıt manifesti](/github/sqliters/target/perf-work/prepare-diagnostics-independent/res/proof.sha256).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1181 geçti, 1 başarısız, 337 yok sayıldı. ON 1172 geçti, 10 başarısız, 337 yok sayıldı. Phase3 uçtan uca testi iki kipte geçti. |
| DML | OFF 380/400, ON 400/400. Ham özetler V6b.76 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet V6b.76 ile aynı. |
| Performans tabanı | 302,53 ms taban, 374,99 ms aday. Süre oranı 1,240 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepare-diagnostics-disk-verify/res/gates-validation.txt).
