# SQLtest için görev sınırı

Kullanıcı bu yeni `sqltest/` dizinine yazmayı açıkça yetkilendirdi. Kardeş
`sqlitesimd/AGENTS.md` dosyasındaki genel salt okunur sınırın yalnız bu görev
kapsamındaki istisnasıdır; üst yönergelerle çatışmada son kullanıcı talimatı
önceliklidir. Motor/C kaynaklarına, kök Cargo dosyalarına veya mevcut kullanıcı
işlerine yazma, stage etme, geri alma yoktur.

**Bu görevde test, DB motoru, SQL execute, derleme ve benchmark yürütme yok.**
`sqlite3`, Python sqlite bağlantısı, Cargo, cc/make, pytest/selftest veya bir
test runner çalıştırmayın. Rust adapter/native C runner oluşturmayın.
Yalnız statik metin/veri generator'ı çalıştırılabilir; subprocess veya SQLite
import'u kullanamaz. Bütün motor sonuçları `NOT_RUN` kalır. Dosya/içerik/hash/
sayı ve determinism incelemesi serbesttir; SQL geçerliliği kanıtı değildir.

Tüm statik çıktılar bu dizinde kalır; büyük generated dosyalar `artifacts/`
altında ignored olur. Kaynak, seed, küçük SQL/CSV/JSONL örnekleri ve manifestler
review edilebilir. Symlink, DB/WAL/SHM/journal, build/lock/binary üretilmez.
Tek aktif alt ajan, ebeveyn inceleme/düzeltme ve adımlar arasında commit kuralı
sürer; alt ajan commit veya sonraki adımı kendi başına başlatmaz.
