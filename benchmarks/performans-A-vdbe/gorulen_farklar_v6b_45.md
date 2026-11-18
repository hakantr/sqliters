# V6b.45 — Açık farklar ve kapsam

D149 için sıcak bölüm başlığı alan sırası, trailer önceliği ve hata dönüşleri ölçülen 203 native akışta eşit. Legacy başlığın sıfır sayfa boyutunda C, açılışta Btree'nin ayarladığı pager boyutunu kullanır. Rust bu ipucunu DB başlığından ayrıca alır. Native ölçüm journal G/Ç izini kapsar. Ana DB başlığı G/Ç çağrılarının sırası, VFS cihaz özelliği sorgulama zamanı ve C tahsis hataları ayrı kanıt gerektiriyor.

D150 sıcak journal sonlandırmasının dizin sync bayrağı, xClose sonucu ve kilit sırası. C `pager.c:2155–2158,2206–2217,3035–3046` dosyayı kapatır, DELETE'e `extraSync` değerini geçirir, SHARED'a inip super-journal temizliğini değerlendirir. Rust sıcak DELETE yolunda `lib.rs:6656–6683` dizin sync ister ve SHARED kilidine dönüş daha sonra gelir. Varsayılan FULL ile EXTRA ve ayrı finalizasyon ve kilit bırakma hataları bir sonraki adımda ölçülecek.

D52, V1/M4, kalan DDL, PRAGMA, savepoint, platform VFS ve E28 kurtarma dalları genel C eşliği için açık. [Karar ve doğrulama](karar_esligi_v6b_45.md).
