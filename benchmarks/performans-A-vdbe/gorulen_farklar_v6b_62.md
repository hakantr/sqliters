# V6b.62 — Ölçülen kapsam ve açık farklar

D166, normal boşta yazılabilir disk WAL kapanışını ve kapanış checkpoint'i sonrası kalıcılık kararını kapsıyor. C VFS'i SHM unmap olayını kaydediyor. Rust bu işlemi ayrı WalShm nesnesi üzerinden yürütüyor ve bu çağrı ayrı gözlem gerektiriyor. HAS_MOVED kararı ve checkpoint hatası açık.

Aktif yazıcı veya okuyucu taşıyan bağlantı kapanışı, EXCLUSIVE ve readonly WAL alt yolları, VFS ve platform kapsamı, Y1 VACUUM bağlantı davranışı ve kalan PRAGMA/M4 açık. [Karar ve doğrulama](karar_esligi_v6b_62.md).

DML seed 6038'de C ve Rust ON, geçerli INSERT OR FAIL ifadesinin kısıt hatasından önce yazdığı iki satırı koruyor. Rust OFF kaynak satırlarını ters sırada okuduğu için kısıt hatasına ilk satırda ulaşıyor. C ile ON dosyaları bayt düzeyinde eşit. OFF farkı C ile yeniden açılan veritabanında da görülüyor. Tarama sırası ayrı bir doğruluk işi.
