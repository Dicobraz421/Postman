# FAST-2: замер gpui-pre против gpui-fast на стенде (Intel iGPU)

lane: deps
status: todo
deps: [FAST-1]
class: B
branch: x/gpui-fast
files: [docs/decisions/2026-11-08-gpui-fast.md]
read: [docs/MEMORY.md#как-мерить]
checks: [bench, release]
manual: no

## Сделать
- Release обоих вариантов (main и `x/gpui-fast`), `postman.exe --demo 50000 --bench`.
- Не меньше 4 чередующихся прогонов на вариант (A B A B …), медианы; машина без нагрузки, монитор тот же.
- Метрики: CPU процесса и UI-потока при листании, CPU в простое, p99/max листания и ресайза, работа UI-потока за
  кадр p50, `idle_private_ws_mb`, первый кадр, размер exe.
- Итог — таблица в `docs/decisions/2026-11-08-gpui-fast.md`, статус «Предложено»; решение — за владельцем.

## Пороги (заданы до прогона)
1. CPU при листании ≤ egui из ADR-GPUI (41–50 %), т. е. условие 2 ADR.
2. CPU в простое ≤ 0,5 %.
3. p99 листания ≤ 9,5 мс, ресайза ≤ 13 мс.
4. Память в простое и размер exe — не хуже main больше чем на 10 %.

## Приёмка
- Файл решения с таблицей, сырые строки бенча приложены под таблицей, по каждому порогу ✅/❌.
