# V6b.80 — V1 bağlantı tanısı C dal kapsamı

C gcov çalışmasında sqlite3Step 4.716, prepare 3.723, reset 2.967 ve transfer 1.002 çağrı. İlk başarısız hazırlama, aktif peer, salt okunur step kesintisi, olumlu hazırlama, positional bind aralığı ve gerçek reset/finalize farklı kararlar olarak yer aldı. [C ham kapsamı ve kaynak](/github/sqliters/target/perf-work/v1-retained-independent/sonuc.md).

Saklı mesaj ve hata ofseti ayrı C durumları. Örneğin başarılı bir TEXT veya NULL bind C result code'u sıfırlarken saklı mesajı korur. Kullanılmamış bir ifadeyi reset veya finalize etmek önceki bağlantı tanısını taşımaz. [12 doğrudan C akışı](/github/sqliters/target/perf-work/v1-error-message-proof/res/c-peers.txt).

Kesin V79 kaynaklarıyla birleşik profilde prepare 3.957, step 4.941, reset 3.138 ve transfer 1.044 çağrı ölçüldü. Truth WHERE altı kez alındı. [Birleşik C kapsamı](/github/sqliters/target/perf-work/v80-v1-independent/sonuc.md).
