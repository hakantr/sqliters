# V6b.35 — görülen farklar ve açık kapsam

- D144'ün dört önceki mmap adı çakışması ile 192 native `prepare_v2` sapması kapandı. Unicode token örnekleri ve soğuk şemadaki erken ad çakışması ham C çıktısıyla eşit. Geçerli DDL'nin yürütümü ve authorizer altında işlem sırası M4 ve API kapsamlarında açık.
- Genişletilmiş mmap matrisinin yedi eski çıktısı aynen sürüyor. Tek bir OFF SELECT sütun adı ofseti ve kesik/bozuk dosya karşılaştırmaları [V6b.34 kaydında](gorulen_farklar_v6b_34.md).
- D143/D57 için başlangıç matrisi native C'de beş, Rust'ta on başlangıç modülü gösteriyor. JSON ve pragma TVF kaydının yaşamı, Hash sırası ve kabuğa özel modüller denetim bekliyor. Sonraki doğruluk işi bu kayıt davranışı. E28, kalan M4 ve önceki yayın engelleri açık.
