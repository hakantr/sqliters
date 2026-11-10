# API test uygulama tarifi — NOT_RUN

Bu dosya runnable adapter değildir; bu görevde C/Rust derleme veya API çağrısı
yapılmaz. Gelecekte ayrı yetki ve aynı kaynak/config/DB girdisi gerekir.

| API yüzeyi | Gelecek action ve assertion |
| --- | --- |
| prepared bind/step | `SELECT ?1,typeof(?1),?2,typeof(?2),?3` hazırla; int64 uç, REAL, null, embedded NUL Text ve empty/00ff BLOB ayrı bind'ler. Sütun tag/byte uzunluğu ve bütün içerik; Done/Row ayrımı; SQL tail gerçek prepare API'siyle, `;` string split değil. |
| reset/rebind | İlk parametrelerle Row/Done; reset dönüşü ve önceki hata durumunu kaydet. Bind değerlerinin reset ile otomatik silindiğini varsayma. İkinci değerle rebind, clear_bindings sonra NULL beklenenleri, yeniden step; eski değer sızıntısı kontrolü. |
| finalize | Başarı ve step hatası sonrası finalize sonucu; resource/drop ve connection close/busy davranışı. Finalize edilmiş handle'ı güvenli Rust'a uydurma veya C use-after-free testi yapma; API misuse sınırı önceden tanımlanmalı. |
| UDF | İki bağımsız bağlantıda callback kaydı, arity/type/null/subtype ve determinism flag; callback hata ve reentrancy/panic politikası kaynakla belirlenir. Portta olmayan API NOT_SUPPORTED + ham C/Rust farkı, noop PASS değil. |
| collation | BINARY/NOCASE ve özel callback'te eşit/ters sıralama; Text byte/encoding, embedded NUL ve callback ömrü. SQL ORDER BY için unique tie-breaker; Türkçe locale semantiği SQLite NOCASE'e icat edilmez. |
| hooks | commit/rollback/update/progress/busy callback event sırası, callback count, kesilme/hata sonrası transaction durumu. Prepare-time ve step-time event farkı; callback yoksa NOOP başarılı sayılmaz. |
| backup | Kaynak snapshot ve ayrı hedef; step sayısı, busy retry/finish, snapshot görünürlüğü ve integrity + typed içerik. Hedef yalnız yeni sandbox'ta; kaynak kullanıcı DB'sine dokunma yok. |
| incremental blob | Gerçek blob rowid handle open/read/write/reopen/close; offset/length boundary ve readonly hatası, boş BLOB, transaction rollback. API desteklenmiyorsa SQL substr ile yapılmış gibi sunulmaz. |
| runtime limits | [Ana plandaki](../docs/uygulama-plani.md) old-value/effective-limit tablosu; API action'ıdır, SQL PRAGMA değildir. |

Her API için parent case, kaynak sembol/hash, gerçek argv/adapter kimliği,
bağlantı/handle yaşam süresi, canonical typed sonuç ve hata evresi gereklidir.
Bu teslimde API desteği veya doğru davranışı doğrulanmadı; **NOT_RUN**.
