# V6b.94 — C MEMORY ve OFF kaynak kiplerinden geçiş

C MEMORY’den WAL’a geçişte önce OFF ara kipini seçiyor. Dört ayrı C profile göre 36 başlık geçişi ölçüldü. MEMORY profillerinde 36 ara OFF kararı ve normal MEMORY önceki yazmada 18 açma/kapatma dalı kaydedildi. Sağlıklı devam ve checkpoint dosya sınırları raporda. [C kaynak ve dal kapsamı](/github/sqliters/target/perf-work/journal-memory-off-independent/sonuc.md)
