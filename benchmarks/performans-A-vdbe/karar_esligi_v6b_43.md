# V6b.43 — D146 sıcak journal seçimi

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_43_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `os_unix.c:6899–6906`, stat sonucu | UNIX xAccess için sıfır baytlık normal dosya bulunmamış sayılır. | `OsVfs::exists`. Boş journal dosyasının bulunduğu dört dolu ve dört boş DB imajı C ile aynı dosya sonuçlarını verdi. |
| `pager.c:5216–5251`, journal var, RESERVED yok, DB boş | C, RESERVED kilidini alabilirse önceki dosyadan kalmış journal'ı kaldırır. Eski sayfalar yazılmaz. | `has_hot_journal` ana dosya boyutunu denetler ve RESERVED aldıktan sonra unlink dener. Dört sayfa boyutunda eski şema geri gelmedi. Sıfır baytlık ana dosyanın hash'i değişmedi. |
| `pager.c:5259–5271`, birinci bayt | İlk bayt `SQLITE_IOERR_SHORT_READ` ile kısa kalırsa C ilk baytı değerlendirip hatayı yutuyor. Diğer okuma hataları aktarılıyor. | `journal_first_byte_is_nonzero`. Enjeksiyonda boş bayt, sonraki deneme ve iki sorgunun çıktısı C ile aynı. |
| `pager.c:5259–5282`, ilk xOpen | Salt okunur MAIN_JOURNAL xOpen birincil CANTOPEN verirse C EXCLUSIVE altında yeniden bakıyor. Diğer hatalar aktarılıyor. | İlk açılışta MAIN_JOURNAL bayrağı kullanılıyor. Dört geometride CANTOPEN sonrası başarılı sıcak recovery ve IOERR sonrası doğru ilk sonuç kodu doğrulandı. |
| `pager.c:5388–5404`, yeniden xOpen | C, EXCLUSIVE altında RW açılış salt okunur tanıtıcı döndürürse CANTOPEN verir. | `play_hot_journal` dönen tanıtıcının niteliğini denetler. C ve Rust ilk sorguyu 14 ile bitirip sonraki sorguda aynı eski veriyi kurtardı. |
| `pager.c:5196–5222`, açık journal tanıtıcısı | C açılmış journal'ı UNDELETABLE_WHEN_OPEN koşulunda kullanır. | Rust bu cihaz özelliği varsa mevcut journal tanıtıcısını okur. Bu platform dalı POSIX probunda alınmadı. |
| `pager.c:7746–7782`, WAL'dan çıkma | Başka okuyucu varken kip değişikliği BUSY döndürür. Son bağlantı kapanınca boş WAL temizlenir. | `close_wal_for_mode_switch` kip sorgusu için aldığı geçici SHARED kilidini yükseltme başarısızsa bırakır. Üst kaynak `walmode-4.13` eski beklentisiyle geçti. |

16 dosya imajı, dört gerçek çökmüş C journal imajı ve 20 native hata akışı iki Rust kipinde C ile eşit. İlk 16 dosya imajında 512, 4096, 8192 ve 65536 sayfa boyutlarıyla boş/dolu DB ve boş/dolu journal birleşimleri kullanıldı. Gerçek çökmüş journal C süreci cache spill sonrasında sonlandırılarak alındı. Aynı journal, yarıda kalan DB ve sıfırlanmış ana dosya ile ayrı ayrı denetlendi. Native probda ilk ve ikinci sorgular, açılış kipi, hata metni ve kodu ile son dosya hash'leri aynı.

V6b.42'nin 88 native DELETE/WAL vakası tekrar eşit. Ham kanıt `/home/hakan/github/sqliters/target/perf-work/v6b46/res/` altında. [Açık farklar](gorulen_farklar_v6b_43.md).

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1143 geçti, 7 başarısız, 337 yok sayıldı. ON 1129 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test kümeleri V6b.42 ile aynı. walmode testi iki kipte geçti. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.41 ile aynı. |
| Ayrışma bulucu | 245 stdout ve 244 stderr sonucu C ile eşit. V6b.42 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 298,99 ms, aday 370,08 ms. Aday/taban 1,238. 14 digest eşit. |

Gcov kapsamındaki 88 C dalının alınmayan 35 çıkışı ayrı kaydedildi.
