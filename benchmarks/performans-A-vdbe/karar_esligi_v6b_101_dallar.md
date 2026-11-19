# V6b.101 — C EXISTS kapsamı ve dış tarama sırası

C'nin iç SELECT'i kendi cursor'ına bağlayan EXISTS kararı, dış satır için bir sütun istemiyor. Normal covering tarama yönü self INSERT veya REPLACE için görünen kaynak sırasını ve son yazılan rowid'yi belirliyor. Dış projeksiyon ve eksik kapsayan indeksler ayrı korundu. [C karar ve ham ölçüm](/github/sqliters/target/perf-work/exists-covering-independent/sonuc.md)
