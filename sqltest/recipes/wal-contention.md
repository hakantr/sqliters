# Ek WAL contention tarifleri — NOT_RUN

Ana dokuz adımlı [schedule](../schedules/wal.json) değiştirilmez. Aşağıdaki
iki alternatif tarif kendi fresh sandbox kopyasında, gerçek ayrı connection
ve ACK orchestrator ile gelecekte yürütülebilir; bu teslimde süreç/SQL yoktur.

1. **Eski snapshot upgrade:** ana schedule'ın setup → reader hold → writer
   commit ilk üç fazını uygula; gerçek WRITER_COMMITTED ACK'sini bekle.
   Sonra aynı reader üzerinde `reader-upgrade.sql` gönder. Beklenti primary
   BUSY (5), extended BUSY_SNAPSHOT (517); ancak hata/evre/transaction durumu
   gerçekte okunmadan UPGRADE_REJECTED ACK'si üretilemez. Ardından aynı reader
   `reader-upgrade-inspect.sql`: rollback öncesi toplam 300, sonra 325 beklenir.
   Bu ayrı dal ana checkpoint/restart sırasına sessizce eklenmez.
2. **İki writer:** setup ve bağlantı preflight'ları sonrası ilk writer'da
   `writer-hold.sql`; başarılı UPDATE ile WRITER_LOCK_HELD ACK'si. İkinci
   writer başka connection/PID'de `competing-writer.sql`; 50 ms busy timeout
   sonrası BUSY beklentisi. Dış step timeout 30 saniye, sonuç beklenmeden
   timeout PASS yapılamaz. COMPETING_WRITER_REJECTED gerçek error ACK'sinden
   sonra ilk writer `writer-release.sql` ile ROLLBACK eder. İkinci bağlantıda
   transaction açılmadığını gelecekte API ile doğrula; başarılı transaction
   varsayıp COMMIT uydurma. Son toplam setup'taki 300 beklenir.

SQL dosyası yorumları gerçek ACK değildir. Busy handler/timeout callback
sayısı, wall-clock bekleme ve extended error modu gelecekte ham artifact'a
bağlanmalıdır. Burada kesin timing veya test sonucu yoktur.
