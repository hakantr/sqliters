# V6b.40 — POSIX kabuk modülleri

POSIX kabuğu gerçek `fsdir` ve `completion` modüllerini C'nin kayıt sırasıyla açar. Native kitaplık profili V6b.39'daki modül kümesini kullanır. Kabuk güvenli kipte C gibi `fsdir` kaydını atlar.

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [Alınan ve alınmayan C dalları](karar_esligi_v6b_40_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kapsam |
|---|---|---|
| `shell.c.in:5243–5250`, kabuk bağlantısı açılıyor | Series, güvenli kip dışındaysa fileio ve ardından completion kaydolur. | `ShellState` açılışı. POSIX normal ve `-safe` koşulları gerçek kabukla denetlendi. |
| `completion.c:198–314,370–414`, önek veya tamamlanmamış satır | Girdinin son ASCII sözcüğü önek olabilir. Adaylar keyword, şema, katalog nesnesi ve sütun aşamalarından gelir. | `CompletionCursor`, C keyword sırasını ve canlı şema sorgularını kullanır. `LIMIT 1` ilk aşamada durduğunda sonraki yardımcı tablo kaydı oluşmaz. |
| `completion.c:424–461`, gizli sütun eşitlikleri | Uygun prefix ve wholeline koşulları sırayla xFilter argümanı olur. | `CompletionTable::best_index` aynı planı üretir. Yazılı koşul, argüman, NULL, boş metin ve tüm satır denetlendi. |
| `fileio.c:654–675,748–808,909–962`, POSIX dosya yürüyüşü | `DIRECTONLY` eponymous tablo, kök ve derinlik için yol bilgisi alır. Dizinler alt alta yürünür. Symlink dizin olarak açılmaz. | `FsDirCursor` symlink metadata, `read_dir` ve sorgunun seçtiği derinlik sınırını uygular. Kök, göreli ön dizin, symlink ve gizli dosyalar denetlendi. |
| `fileio.c:222–253,812–881,977–1070`, veri sütunu ve kısıtlar | Dizin verisi NULL, symlink verisi hedef metni, dosya verisi BLOB olur. Uzunluk sınırı aşılırsa `SQLITE_TOOBIG`. | `FileEntry::data` ve `FsDirTable::best_index`. `.limit length`, seçilmeyen FIFO veri sütunu, dosya türleri, seviye ve erişim tanıları denetlendi. |

Kapsanan CLI akışlarında veri, hata/uyarı ve dosya hash'leri birlikte karşılaştırıldı. 36 completion, 42 fsdir ve 12 modül profili akışının tamamı eşit. [Açık farklar](gorulen_farklar_v6b_40.md).

Ham kanıt dizini `/home/hakan/github/sqliters/target/perf-work/v6b42/`. `completion_check.py`, `fsdir_check.py`, `shell_coverage.py`, `check.sh` ve `gates.sh` ölçümleri üretir. `res/source_manifest.json`, `res/oracle_and_binaries.json`, `res/validation.txt` ve `res/verified_commit.txt` gözlemleri sürüme bağlar.

| Yayın kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1141 geçti, 4 başarısız, 337 yok sayıldı. ON 1127 geçti, 18 başarısız, 337 yok sayıldı. Başarısız test kümeleri V6b.39 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.39 ile aynı. |
| Ayrışma bulucu | 245 senaryonun stdout'u ve 244 senaryonun stderr'i C ile eşit. Önceki özet bayt düzeyinde aynı. |
| Rust 1.100 tabanına hız, yedi deneme | Toplam medyan taban 299,16 ms, aday 367,01 ms. Aday/taban 1,227. 14 digest eşit. |

Native modül 46, D103 184, üretilen sütun 244 ve önceki DDL/mmap/TEMP matrislerinin ham sonuçları değişmedi. C kapsamı 214 dal ve ayrı sınıflandırılan 43 alınmayan çıkış.
