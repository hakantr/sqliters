# V6b.191 — Kapanış sonucu ve dosya ömrü

Yan dosya temizliğinin dizin tarafından reddedildiği ölçümde C ve aday aynı iki silme çağrısını yaptı. Dizinde yazılamayan WAL biçimi ve SHM yerinde kaldı. Yazılabilir dizin kontrolünde iki motor hedef rollback başlığına geçti. 512, 4096 ve 65536 sayfa, V1/V2 ve beş hedef kip her iki koşulda doğrulandı. [Fiziksel evre matrisi](/github/sqliters/target/perf-work/v189-wal-directory-cleanup.oViPrzq6/res/compare-full/cases.json)
