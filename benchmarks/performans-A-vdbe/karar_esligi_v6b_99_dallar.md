# V6b.99 — C scalar MAX yürütme sınırı

`sqlite3MaterializeView` ve sonraki özgün WHERE, C'nin ayrı Once alanları. Normal eşitlikte iki MAX sonuçlandırması, boş hedefte bir ve prepared reset sonrası iki yeni değerlendirme gözlendi. Geçici değerlendirme izleri exact optimize adayın ayrı ham ölçümünden ayrıdır. [C kaynak ve kapsam](/github/sqliters/target/perf-work/view-max-v98-independent/sonuc.md)
