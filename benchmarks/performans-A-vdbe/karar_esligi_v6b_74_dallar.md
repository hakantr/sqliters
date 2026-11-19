# V6b.74 — F15 için kullanılan C kapsamı

V6b.41 ERROR ve V6b.44 faz iki arşiv profilleri kullanıldı. Yalnız test beklentisi güncellendi. Yeni C arıza koşusu yapılmadı.

- `pager.c:6764–6801` faz iki guard'ı ERROR sırasında saklı kodu taşır. Arşiv probu ilk faz iki hatasını ölçer. Arşivdeki `PAGER_RETRY` faz bir çağrısıdır.
- `pager.c:6838–6873` rollback, aynı ERROR kodunu taşır. Arşivdeki dört doğrudan C ölçümünde bu çıkış alındı.
- Taze salt okunur ilk açılış, C ile Rust'ta tutulan bütün dış sayfa referansı bileşimleri, farklı VFS cihaz kipleri ve sync kaybı sonrası yeniden açılış bu test beklentisi için ek kanıt sayılmadı.
- C kaynak düzeyinde sıcak journal okunması, temiz kip geçişi ve ERROR guard'ının açıklaması bağımsız raporda.

[C ve kanıt sınırları](/github/sqliters/target/perf-work/f15-expectation-audit/sonuc.md) · [V6b.41 C kapsamı](/github/sqliters/.claude/worktrees/vdbe/benchmarks/performans-A-vdbe/karar_esligi_v6b_41_dallar.md).

F14 `pager.c:1890–1973,2240–2267,5533–5538` son referans bırakıldığında ERROR çıkışını ve sonraki journal kurtarmasını kapsayan önceki arşivden yararlandı. `write-persistent / reuse` C kaydı sabit SELECT, aynı bağlantıda tablo okuma ve yeni bağlantı bütünlük sonucunu içeriyor. F14 fixture'ı bağlantı yeniden kullanımını, F15 fixture'ları pager hata ömrünü ayrı ölçer.
