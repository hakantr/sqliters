# V6b.64 — Açık REGEXP eşlik maddeleri

Bu adım SELECT ve SELECT içeren EXPLAIN hazırlamasındaki iki REGEXP hatasını kapsıyor. Stored view üzerinden hazırlama 6/6 eşit. Dış INSERT içinde tetikleyiciyle derlenen REGEXP tanısı, legacy kipte hâlâ küçültülmüş fonksiyon adı döndürüyor. Tetikleyici karşılaştırmasında OFF 0/6, ON 2/6 ortak iz eşliği var. 12/12 view ve trigger vakasında dosya özetleri eşit.

Kayıtlı callback'in 7 veya 9 döndürdüğü pozitif infix REGEXP için legacy yürütme bunu 1'e çeviriyor. Ham değeri ve NOT/NULL kontrollerini içeren 24 vakada OFF 12/24, ON 16/24 eşit. Bu yürütme farkı ve MATCH V1 step/reset yaşam döngüsü açık. [Karar ve doğrulama](karar_esligi_v6b_64.md).

Depolama ve kurtarma kapsamındaki önceki açıklar V6b.63 kaydında. [V6b.63 açıkları](gorulen_farklar_v6b_63.md).
