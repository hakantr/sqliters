# V6b.49 — Açık farklar ve kapsam

D153 olağan disk rollback-journal kapanışında ölçülen başarılı sync, boyut, kilit ve dosya tanıtıcısı sırası C ile eşit. Aktif yazıcı ve WAL kapanışı, ERROR veya fiziksel xClose başarısızlığı, Rust'ın doğrudan `Pager::close` hata sonucu ile C terminal API sonucu ayrı doğrulama gerektiriyor.

D154 normal rollback-journal COMMIT sonu SHARED. C `pager.c:2209–2217` sonlandırmadan sonra SHARED'a döner, son okur bırakılınca NO_LOCK yapılır. Önceki kayıtlar V6b.49'da serbest okur halinde Rust'ın ayrı SHARED çağrısının eksik olduğunu gösterir. D155 `pager.c:2204–2207` COMMIT_PHASETWO bildirimi C'de sonlandırma ve SHARED arasında yer alır. Ham kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b54-proof/res/` altında.

D156 disk rollback journal sayfa kayıt yazımı. C `pager.c:6078–6093` numara, sayfa, checksum alanlarını ayrı yazar. Rust bir kayıt için tek yazma kullanır. D52, V1/M4, kalan DDL, PRAGMA ve diğer pager ve WAL yolları genel C eşliği için açık. [Karar ve doğrulama](karar_esligi_v6b_49.md).
