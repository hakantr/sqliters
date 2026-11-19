# V6b.72 — Bağlantı kesintisi ve hazırlama sırası

Referans C SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0.

| C kararı | Rust davranışı |
|---|---|
| `tokenize.c:616–618`, `vdbeapi.c:855–867` | Etkin ifade yokken varsayılan kesinti bayrağı sonraki hazırlama veya ilk step'te temizleniyor. Etkin okuyucu kesintiyi görüyor ve kiraladığı kaynakları bırakıyor. |
| `tokenize.c:650–668`, `prepare.c:788–842` | Özel tokena kadar ulaşan hazırlama rc9 döndürüyor. Parser daha erken sözdizimi hatasına veya geçerli bitişe ulaşırsa onu koruyor. Çıplak kesinti önceki ofseti taşıyor. |
| `vdbe.c:9501–9510`, `vdbeapi.c:951–956` | V1 okunabilir ifade step'te rc1 ve reset/finalize'da gerçek rc9'u döndürüyor. V2 yürütme hatasını step'te de gösteriyor. |
| `vtab.c`, `sqlite3VtabEponymousTableInit` ve `sqlite3_declare_vtab` | Hazırlama yoklaması gerçek constructor sınırını gözlüyor. Normal yol constructor'ı bir kez çağırıyor. Soğuk JSON constructor ve PRAGMA declaration hata metni C ile eşit. Sıcak bağlantı tekrar constructor çağırmıyor. |

Bağlı diskte kesin V6b.71 kaynaklarıyla 566 native vaka yeniden çalıştı ve önceki ham kayıtlarla eşleşti. Lexical 276/276, sıcak/soğuk vtab 12/12 ve giriş korumaları 6/6 iki kipte C ile aynı. Karma 32 vakada OFF 31/32 ve ON 32/32. 240 yaşam akışında iki kip 210/240. Eski V1 bağlantı hata metni ve ofseti 30 akışta açık. Ölçülen dosya SHA-256 değerleri eşit. [Bağımsız native yeniden koşu](/github/sqliters/target/perf-work/interrupt-resume-independent/res/replayed.txt).

Rust'ın çağıran tarafından sağlanan AtomicBool davranışı kesin V6b.71 tabanıyla 12/12 eşit. Group46'nın default idle beklentisi C'nin belgelediği dört satıra göre güncellendi. ON'daki eski custom flag başarısızlığı kendi assertion'ında açık. Group3 ve beş odak regresyonu iki kipte başarılı. [OFF odak](/github/sqliters/target/perf-work/interrupt-resume-independent/res/focused-phase1-off.txt) · [ON odak](/github/sqliters/target/perf-work/interrupt-resume-independent/res/focused-phase1-on.txt) · [Harici bayrak izleri](/github/sqliters/target/perf-work/interrupt-resume-independent/res/custom-arc-fresh.json).

Public `ExecContext`, codegen `Db` ve `CoreVTabHostContext` alanları aynı. Yama ve kaynak özetleri bağımsız koşuyla doğrulandı. [Kaynak manifesti](/github/sqliters/target/perf-work/interrupt-resume-independent/res/candidate-patch-files.sha256) · [Ham çıktı manifesti](/github/sqliters/target/perf-work/interrupt-resume-independent/res/resume.sha256).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1176 geçti, 4 başarısız, 337 yok sayıldı. ON 1164 geçti, 16 başarısız, 337 yok sayıldı. Group3 ve beş yeni test iki kipte geçti. |
| DML | OFF 380/400, ON 400/400. Ham özetler V6b.71 ile aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet V6b.71 ile aynı. |
| Performans tabanı | 298,34 ms taban, 372,27 ms aday. Süre oranı 1,248 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/home/hakan/github/sqliters/target/perf-work/interrupt-disk-verify/res/gates-validation.txt).
