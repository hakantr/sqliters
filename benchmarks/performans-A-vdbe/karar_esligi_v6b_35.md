# V6b.35 — D144 DDL adı hazırlama tanısı

Bir `CREATE TABLE`, `CREATE TABLE AS` veya `CREATE VIEW` adı çakıştığında hata iletisi C'nin token yazımını ve bayt ofsetini korur. Bu kapsamda bilinmeyen şema adının token ve bayt ofseti de eşlendi. İlk dört mmap matrisi hatasında Rust ofseti -1 yerine C gibi 13 döner.

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [64 C dalı ve ayrı sınıflandırılan 18 alınmayan çıkış](karar_esligi_v6b_35_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kapsam |
|---|---|---|
| `build.c:1046–1063`, nitelikli ad | Var olan şemada ikinci token nesne adıdır. Şema yoksa ilk token ile `unknown database %T` oluşturulur. | `build_tab.rs::sqlite3_two_part_name` ve `sqliters-core/src/lib.rs:6107`. Ana, TEMP, bağlı ve bilinmeyen şema girdileri. |
| `build.c:1337–1353`, şema okunur ve tablo/görünüm bulunur | `IF NOT EXISTS` yoksa mevcut nesnenin türü ve ham ad token'ı hataya girer. `IF NOT EXISTS` ile şema doğrulanır ve erken çıkılır. | `c_create_name_error` mevcut Rust hata yolunda C üreticisinin tanısını alır. Normal başarılı hazırlama/yürütme mevcut yoldan geçer. İndeks çakışması ve no-op girdileri negatif kontrol. |
| `printf.c:1085–1091`, dahili `%T` | Tırnak ve yazım dahil token baytları eklenir. Ofset SQL'in başından ölçülür. | `sqlite3_start_table` içindeki `tok_str` ve `error_msg_tok`. Köşeli, çift ve ters tırnak, ASCII dışı ad ve yorum baytları kontrol edildi. |
| `parse.y:207–228`, tablo adı kolon listesinden veya AS SELECT'ten önce indirgenir | C erken çakışmayı sonradan gelen bozuk kolon listesinden önce görür. İlk schema read de bu sırada gerçekleşebilir. | `starts_create_table` ve `compile_in_session` içindeki şema yükleme kararı. Rust parser reddederse C üreticisinin şema okuma sinyali yeniden açılmış dosyada önce gerçek şemayı yükletir, ardından C tanısı tekrar alınır. |
| `build.c:1313–1327`, authorizer | Yetkilendirme namespace çakışma denetiminden önce olabilir. | Yeni tanı köprüsü authorizer etkin bağlantıda devreye girmez. Kullanıcı callback sırası ve CREATE VIRTUAL TABLE bu adımın açık kapsamı. |

928 native hazırlama akışı eşit. `sqlite3_prepare_v2` ve `sqlite3_prepare` için ayrı ayrı 464 akış çalıştı. V6b.34 ikilisiyle `prepare_v2` matrisinde bulunan 192 sapma kapanmış durumda. Testler sıcak ve yeniden açılan dosyayı, şema nitelemesini, tablo/görünüm çakışmalarını, `IF NOT EXISTS`, AS SELECT ve bozuk kolon listesini içerir. Native akışlarda exit, stdout, stderr ve bütün dosya hashleri birlikte kıyaslandı. Elle CLI denetimleri 14/14.

Önceki mmap matrisi 250/250. Genişletilmiş mmap 433/440 ve yedi ham fark V6b.34 ile aynı. Büyüme/geometri 52/52, TEMP 270/270, tam mmap programı 144/144, etkin okuyucu snapshot ve EXPLAIN 12/12. [Açık farklar](gorulen_farklar_v6b_35.md).

Ham kanıt dizini `/home/hakan/github/sqliters/target/perf-work/v6b35/`. `ddl_conflicts.py`, `manual_ddl.py`, `ddl_proof.py`, `check.sh` ve `gates.sh` ölçümleri tekrar üretir. `res/source_manifest.json`, `res/oracle_and_binaries.json`, `res/validation.txt` ve `res/verified_commit.txt` doğrulama kaynaklarını sürüme bağlar.

| Yayın kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1136 geçti, 4 başarısız, 337 yok sayıldı. ON 1122 geçti, 18 başarısız, 337 yok sayıldı. İki kipte de başarısız test adları V6b.34 ile aynı. |
| DML, 6001–6400 | OFF 378/400, ON 398/400. Ham seed JSON'ları V6b.34 ile aynı. |
| Ayrışma bulucu | 245 senaryonun stdout'u ve 244 senaryonun stderr'i C ile eşit. V6b.34 özeti bayt düzeyinde korundu. |
| Rust 1.100 tabanına hız, yedi deneme | Toplam medyan taban 297,52 ms, aday 367,58 ms. Aday/taban 1,235. 14 digest eşit. |
