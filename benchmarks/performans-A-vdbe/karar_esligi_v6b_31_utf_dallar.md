# V6b.31 — UTF, NOCASE ve sayısal hücre C kapsamı

Ham hücre profili SQLite 3.54.1 kaynak kopyasını `-O0 --coverage` ile doğrudan içerir; kaynak baytları ana oracle ile aynı. SQL/API profili ana doğrulama nesnesidir. Ayrı gcov derlemelerinin sayaçları birleştirilmedi. Her tablo satırı kendi profiline aittir. Rust 1.100.0. Genel NOMEM/tahsis izi ve tüm geçersiz TEXT değerlerinin uygulama API temsili kanıtlanmış sayılmaz.

| Profil | C yordamı | Çalışan satır | Alınan dal |
|---|---|---|---|
| ham hücre | utf.c / sqlite3VdbeMemTranslate | 62/64 | 72/74 |
| ham hücre | main.c / nocaseCollatingFunc | 5/5 | 2/2 |
| ham hücre | main.c / rtrimCollFunc | 6/6 | 8/8 |
| ham hücre | util.c / sqlite3_strnicmp | 7/9 | 10/14 |
| ham hücre | util.c / sqlite3Atoi64 | 51/51 | 54/54 |
| ham hücre | util.c / sqlite3AtoF | 75/75 | 57/58 |
| ham hücre | vdbeaux.c / vdbeCompareMemString | 4/4 | 2/2 |
| ham hücre | vdbeaux.c / vdbeCompareMemStringWithEncodingChange | 12/14 | 2/6 |
| ham hücre | vdbemem.c / sqlite3VdbeChangeEncoding | 5/8 | 2/4 |
| ham hücre | vdbemem.c / sqlite3VdbeIntValue | 6/9 | 4/8 |
| ham hücre | vdbemem.c / memIntValue | 4/4 | 0/0 |
| ham hücre | vdbemem.c / sqlite3MemRealValueRC | 9/11 | 6/10 |
| ham hücre | vdbemem.c / sqlite3MemRealValueRCSlowPath | 20/25 | 14/18 |
| ham hücre | vdbemem.c / sqlite3VdbeMemNumerify | 10/10 | 7/8 |
| ham hücre | vdbe.c / applyNumericAffinity | 9/9 | 7/8 |
| SQL/API | vdbemem.c / sqlite3VdbeMemCast | 27/27 | 13/15 |
| SQL/API | vdbe.c / applyAffinity | 11/13 | 12/16 |

Toplam **33 alınmayan dal** aşağıda ayrı sınıflandırılmıştır. Aynı koşulun kısa devre çıkışları birleştirilmez. Bütün alınan/alınmayan dallar `res/utf_branches.json` içinde korunur.

| Profil / C yeri | Dal | C koşulu | Sınıflandırma | Rust yeri |
|---|---|---|---|---|
| ham hücre / `utf.c:270` | branch  0 taken 0 (fallthrough) | `if( rc!=SQLITE_OK ){` | UTF16 endian takasında MakeWriteable NOMEM yönü enjekte edilmedi; Rust Vec tahsis başarısızlığı/hata dönüşü eşliği açık. | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:313` | branch  0 taken 0 (fallthrough) | `if( !zOut ){` | Dönüşüm çıkış tamponu malloc NULL yönü enjekte edilmedi; Rust Vec tahsis politikası C allocator iziyle eşitlendi sayılmaz. | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `util.c:444` | branch  0 taken 0 (fallthrough) | `if( zLeft==0 ){` | Genel strnicmp NULL sol işaretçisi bu ölçümde yok; Rust karşılaştırıcı &slice alır. İç NOCASE çağrısı dışında public NULL API eşliği kanıtlanmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:445` | branch  0 never executed (fallthrough) | `return zRight ? -1 : 0;` | NULL sol girdi sonrası sağın NULL/NULL olmayan iki çıkışı ayrı ayrı alınmadı; Rust slice temsili public NULL API kabulü sağlamaz. | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:445` | branch  1 never executed | `return zRight ? -1 : 0;` | NULL sol girdi sonrası sağın NULL/NULL olmayan iki çıkışı ayrı ayrı alınmadı; Rust slice temsili public NULL API kabulü sağlamaz. | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:446` | branch  0 taken 0 (fallthrough) | `}else if( zRight==0 ){` | Genel strnicmp NULL sağ işaretçisi alınmadı; Rust &slice girişinin dışındaki API yönü açık. | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:960` | branch  1 taken 0 | `exp = exp<10000 ? (exp*10 + v) : 10000;` | Üs biriktirmesi 10000 doygunluğunu aşan rakam yönü bu sayısal matrisin dışında; parserdeki karşılığı mevcut olsa da bu dal ayrıca kanıtlanmadı. | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `vdbeaux.c:4442` | branch  1 taken 0 | `if( (v1==0 \|\| v2==0) ){` | Geçici kodlama dönüşümlerinden v1/v2 NULL (NOMEM) yönleri enjekte edilmedi; Rust dönüşümünde genel tahsis hatası yayılımı açık. | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4442` | branch  2 taken 0 (fallthrough) | `if( (v1==0 \|\| v2==0) ){` | Geçici kodlama dönüşümlerinden v1/v2 NULL (NOMEM) yönleri enjekte edilmedi; Rust dönüşümünde genel tahsis hatası yayılımı açık. | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4443` | branch  0 never executed (fallthrough) | `if( prcErr ) *prcErr = SQLITE_NOMEM_BKPT;` | Dönüşüm NOMEM sonrası prcErr var/yok çıkışları alınmadı; bu genel C hata işaretçisi Rust karşılaştırıcı API’sinde eşitlendi sayılmaz. | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4443` | branch  1 never executed | `if( prcErr ) *prcErr = SQLITE_NOMEM_BKPT;` | Dönüşüm NOMEM sonrası prcErr var/yok çıkışları alınmadı; bu genel C hata işaretçisi Rust karşılaştırıcı API’sinde eşitlendi sayılmaz. | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbemem.c:220` | branch  0 taken 0 (fallthrough) | `if( !(pMem->flags&MEM_Str) ){` | Ham hücre profili yalnız MEM_Str girdi verir; STR olmayan hücrede encoding no-op guardı bu profilde ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:276` |
| ham hücre / `vdbemem.c:224` | branch  0 taken 0 (fallthrough) | `if( pMem->enc==desiredEnc ){` | Ham profilde ChangeEncoding çağrısı yalnız farklı kodlama gerektiğinde yapılır; aynı kodlamadaki erken dönüş bu profilde ayrıca kanıtlanmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:276` |
| ham hücre / `vdbemem.c:647` | branch  0 taken 0 (fallthrough) | `if( flags & (MEM_Int\|MEM_IntReal) ){` | Ham sayısal profil TEXT girişlidir; önceden MEM_Int/MEM_IntReal tutan hücrenin doğrudan dönüşü bu profilde ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:650` | branch  0 taken 0 (fallthrough) | `}else if( flags & MEM_Real ){` | Ham sayısal profil TEXT girişlidir; önceden MEM_Real tutan hücrenin dönüşü bu profilde ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:652` | branch  1 taken 0 | `}else if( (flags & (MEM_Str\|MEM_Blob))!=0 && pMem->z!=0 ){` | Ham sayısal profilde hücre TEXT ve z tamponu geçerlidir; NULL/diğer tür veya NULL z kısa devre yönleri ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:652` | branch  3 taken 0 | `}else if( (flags & (MEM_Str\|MEM_Blob))!=0 && pMem->z!=0 ){` | Ham sayısal profilde hücre TEXT ve z tamponu geçerlidir; NULL/diğer tür veya NULL z kısa devre yönleri ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:738` | branch  0 taken 0 (fallthrough) | `if( pMem->z==0 ){` | Ham profil geçerli tampon (boşsa sıfır uzunluk) verir; NULL z girişinin C kontrolü ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:742` | branch  0 taken 0 (fallthrough) | `&& ((pMem->flags & MEM_Term)!=0 \|\| sqlite3VdbeMemZeroTerminateIfAble(pMem))` | Ham profilde MEM_Term vardır; sonlandırılmamış UTF8 tamponunun ZeroTerminateIfAble başarı/hata kısa devre yönleri ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:742` | branch  3 never executed (fallthrough) | `&& ((pMem->flags & MEM_Term)!=0 \|\| sqlite3VdbeMemZeroTerminateIfAble(pMem))` | Ham profilde MEM_Term vardır; sonlandırılmamış UTF8 tamponunun ZeroTerminateIfAble başarı/hata kısa devre yönleri ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:742` | branch  4 never executed | `&& ((pMem->flags & MEM_Term)!=0 \|\| sqlite3VdbeMemZeroTerminateIfAble(pMem))` | Ham profilde MEM_Term vardır; sonlandırılmamış UTF8 tamponunun ZeroTerminateIfAble başarı/hata kısa devre yönleri ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:683` | branch  0 taken 0 (fallthrough) | `if( pMem->enc==SQLITE_UTF8 ){` | UTF8 ham girdiler MEM_Term olduğu için UTF8 slow-path girişi alınmadı; sonlandırılmamış hücre tahsisi ayrıca kanıtlanmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:685` | branch  0 never executed (fallthrough) | `if( zCopy ){` | UTF8 sonlandırılmamış slow-path’e girilmediği için kopya tahsisi başarı/NULL çıkışları alınmadı; genel allocator izi açık. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:685` | branch  1 never executed | `if( zCopy ){` | UTF8 sonlandırılmamış slow-path’e girilmediği için kopya tahsisi başarı/NULL çıkışları alınmadı; genel allocator izi açık. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:697` | branch  1 taken 0 | `if( zCopy ){` | UTF16 geçici ASCII kopyasında malloc başarısızlığı enjekte edilmedi; NOMEM yönü açık. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:899` | branch  1 taken 0 | `if( (pMem->flags & (MEM_Int\|MEM_Real\|MEM_IntReal\|MEM_Null))==0 ){` | Ham Numerify girişleri TEXT’tir; önceden numeric/NULL türünde hücrenin dönüşümü atlama yönü bu profilde ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbe.c:365` | branch  1 taken 0 | `if( bTryForInt ) sqlite3VdbeIntegerAffinity(pRec);` | Ham affinity yoklaması bTryForInt=true kullanır; false yönü bu profilde ayrıca alınmadı. | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| SQL/API / `vdbemem.c:933` | branch  1 taken 0 | `if( pMem->flags & MEM_Str ) MemSetTypeFlag(pMem, MEM_Blob);` | BLOB CAST sırasında stringify sonrası STR oluşmaması tahsis hatası gerektirir; NOMEM enjekte edilmedi, Rust hata yayılımı açık. | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:961` | branch  0 taken 0 (fallthrough) | `if( rc ) return rc;` | CAST kodlama dönüşümünden hata (NOMEM) dönüşü enjekte edilmedi; genel tahsis/hata eşliği açık. | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbe.c:406` | branch  1 taken 0 | `if( (pRec->flags & (MEM_Real\|MEM_IntReal))==0 ){` | SQL/API profilinde affinity’nin önceden REAL/INTREAL hücre yönü bu noktada alınmadı; genel affinity kapsamı tamamlandı sayılmaz. | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:407` | branch  0 taken 0 (fallthrough) | `if( pRec->flags & MEM_Str ) applyNumericAffinity(pRec,1);` | SQL/API profilinde bu numeric-affinity dalında STR girdisi yönü ayrıca alınmadı; ham numeric profilinin sonucu ayrı tutulur. | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:408` | branch  0 never executed (fallthrough) | `}else if( affinity<=SQLITE_AFF_REAL ){` | SQL/API profilinde önceden REAL/INTREAL girdiden affinity<=REAL ayrımı iki yönde de ayrıca alınmadı; genel affinity eşliği açık. | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:408` | branch  1 never executed | `}else if( affinity<=SQLITE_AFF_REAL ){` | SQL/API profilinde önceden REAL/INTREAL girdiden affinity<=REAL ayrımı iki yönde de ayrıca alınmadı; genel affinity eşliği açık. | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
