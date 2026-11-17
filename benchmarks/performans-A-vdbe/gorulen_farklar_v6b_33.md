# V6b.33 — görülen farklar ve açık kapsam

Unix VFS fetch/unfetch limit/lease altyapısındaki 10.051 işlem C ile aynıdır. SQL pager fetch bağlantısı henüz yoktur: native mmap matrisinde **D142 altı** ve **D144 dört** fark sürer; 240/250 tam aynı, bütün kalıcı dosyalar aynı. Farklar normalize edilerek geçen sayılmadı.

C mmap başarısızlık günlüğü eksiktir; mevcut haritayı uzatma/mremap, fstat hata enjeksiyonu, NOMEM ve Windows fetch yolu tamamlandı/kanıtlandı sayılmaz. Başarı kapsamı bu eksikleri kapatmaz. Yeni CLI16, TEMP270 ve tam program144 karşılaştırmaları aynı; dört kapıda önceki hata kümeleri korundu.

D143/D57 module_list, D52, D103, genel E27/E28, eski C dışı test beklentileri ve kalan V6b/M4 açık. Sonraki iş **V6b.34 / D142 sayfalayıcı mapped-page/ref yaşamı**. Kullanıcı sohbet devri istedi; yeni işe bu sohbette başlanmadı. [Karar/kanıt](karar_esligi_v6b_33.md) ve [devam belgesi](devir_v6b_33.md).
