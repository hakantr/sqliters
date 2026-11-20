# V6b.192 — Kapanış kabulü ve kaynak bırakma

C `sqlite3Close` önce canlı kaynak denetimini yapıyor, ardından terminal kapanışı başarı olarak dönüyor. Kapanıştan önce ve sonra 768 DB ve yan dosya görüntüsü, aynı kullanıcının okuyabildiği veri ve fiziksel WAL sonucu eş. NO_CKPT_ON_CLOSE ve soğuk/sıcak bağlantı kontrolleri işlendi. [C pager kapanışı](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/c/sqlite3.c:63189) · [Native matris](/github/sqliters/target/perf-work/v191-connection-close-result.dDXTiIo4/res/adjusted-full/cases.json)
