# V6b.49 — D153 olağan rollback-journal bağlantı kapanışı

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_49_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:4254–4272,4102–4108`, olağan disk kapanışı | Açık rollback journal önce xSync ve xFileSize görür. OFF fiziksel xSync'i atlar, boyut okuması kalır. Açık journal bulunmuyorsa iki çağrı da yapılmaz. | `finish_idle_rollback_close` (`lib.rs:6308`) `sync_hot_journal` (`lib.rs:6723`) yordamını bir kez çağırır. Sağlıklı 18 vakada açık ve kapalı tanıtıcı dalları ve OFF seçimi kesin izde eşit. |
| `pager.c:2240–2249,2109–2112,2209–2217`, tutulan okur ve kilit | EXCLUSIVE kapanışa normal kip olarak girer. Tutulan okur ve RESERVED veya üstü kilit varsa mevcut journal sonlandırması ve SHARED denenir. Önceden temiz PERSIST/TRUNCATE tanıtıcısı için boyut değişikliği oluşmaz. | `lib.rs:6313–6320` fiziksel kilidi ve tutulan okur durumunu birlikte denetler. EXCLUSIVE kontrol izlerinde sync ve SHARED, PERSIST/TRUNCATE tanıtıcısı kapanmadan önce gelir. EXCLUSIVE DELETE temiz journal bir kez sync edilip silinir. |
| `pager.c:1915–1940,4278–4279`, journal ömrü ve NO_LOCK | Normal cihazda journal NO_LOCK öncesinde kapanır. `UNDELETABLE_WHEN_OPEN` olan PERSIST/TRUNCATE tanıtıcısı NO_LOCK sonrasına kalır. Ana DB xClose sonlandırma dizisinin sonunda gelir. | `close_journal_before_unlock` (`lib.rs:4962`), `finish_idle_rollback_close` ve `close` (`lib.rs:6230`) aynı sıraları uygular. C ve iki Rust kipinde journal xSync, xFileSize, SHARED/NO_LOCK, journal ve DB xClose, SQL sonuçları ve kalıcı dosya özetleri eşit. |

Bağımsız 18 sağlıklı native vaka, PERSIST ve TRUNCATE için OFF, NORMAL ve FULL altında normal, UNDELETABLE_WHEN_OPEN ve EXCLUSIVE bağlantı kapanışlarını kapsar. Gerçek çalışma ağacının ikilisiyle önceki 24 kurtarma ve sekiz cihaz yeteneği vakası kapanış dahil tekrar eşit. Dört EXCLUSIVE PERSIST/TRUNCATE ve dört EXCLUSIVE DELETE kontrolü ayrı scratch adayında eşit. D152 sync bildiriminin 20 vakası aynı kaldı. Ham kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b53-proof/`, `/home/hakan/github/sqliters/target/perf-work/v6b53_pager/` ve `/home/hakan/github/sqliters/target/perf-work/v6b53_verify/` altında. [Açık farklar](gorulen_farklar_v6b_49.md).

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1147 geçti, 7 başarısız, 337 yok sayıldı. ON 1133 geçti, 21 başarısız, 337 yok sayıldı. Yeni EXCLUSIVE PERSIST kapanış testi iki kipte geçti. Başarısız kümeler V6b.48 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.48 ile aynı. |
| Ayrışma bulucu | C ile 245 stdout ve 244 stderr sonucu eşit. V6b.48 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 298,13 ms, aday 369,60 ms. Aday/taban 1,240. 14 digest eşit. |

Önceki 147 kurtarma, 12 journal imajı ve dört gerçek C çöküş vakası eşit. Gcov ana scope ve sync yardımcısında 32 çıkışın 19'unu aldı, kalan 13 çıkış sınıflandırıldı.
