# Инженерные навыки

[English](../engineering-skills.md) · [Русский](engineering-skills.md)

Инженерные навыки лежат рядом со служебными в `skills/` и входят в существующую
упаковку дистрибутива. Каждый каталог содержит самостоятельный `SKILL.md`:
короткое условие применения и процедуру с проверяемым результатом. Инструкции
написаны на английском, как существующие skills; агент по-прежнему может отвечать
на языке пользователя.

## Как выбирать навыки

Базовая последовательность для изменения кода: `problem-analysis` →
`repository-investigation` → реализация с `minimal-change` → `verification`.
Масштаб процедуры зависит от задачи: небольшой правке достаточно нескольких
установленных фактов и прямой проверки, без отдельного отчёта на каждом этапе.
Уже собранные доказательства используются повторно.

Специализированный навык загружается, когда подходит его условие применения:

- Повторяющийся дефект: `root-cause-analysis`; `debugging-and-profiling` помогает
  собрать диагностику, а `testing` — сделать содержательную регрессионную проверку.
- Новый API: `requirements-engineering`, `api-design`, затем реализация и
  необходимые проверки совместимости.
- Изменение схемы под нагрузкой: `database-engineering` и
  `migration-engineering`; конкурентность и распределённые протоколы разбираются,
  если переход действительно их затрагивает.
- Пользовательский сценарий: `ui-ux-design`; для отдельного аудита или исправления
  доступности — `accessibility`.
- Выкатка в production: `release-engineering` и `production-readiness` вместе с
  принятой платформенной процедурой, например `rust-release` для Rust.

Это рекомендуемый порядок работы, а не принудительная последовательность в движке.
Rook показывает карточки, а модель решает, когда вызвать `load_skill`. Добавление
файлов не превращает четыре базовых навыка в обязательные системные инструкции;
для этого нужно отдельное изменение политики агента. Навык сам по себе не даёт
разрешение на деплой, публикацию, переписывание истории или отправку сообщений.

Repository Exploration и Repository Investigation объединены в один навык;
Implementation Verification и Verification — тоже. Requirements описывает
требуемое поведение, problem analysis связывает его с существующей системой.
Testing создаёт полезные проверки, verification оценивает выполнение всей задачи.
Refactoring меняет структуру, cleanup убирает ненужные остатки текущей реализации.
Такие границы позволяют не загружать эквивалентные процедуры дважды.

## Каталог

### Анализ и управление изменением

| Навык | Для чего применять |
|---|---|
| [problem-analysis](../../skills/problem-analysis/SKILL.md) | Цель, ограничения, текущее поведение, неизвестные, риски и проверки |
| [repository-investigation](../../skills/repository-investigation/SKILL.md) | Точки входа, похожие реализации, тесты, соглашения и пути зависимостей |
| [requirements-engineering](../../skills/requirements-engineering/SKILL.md) | Поведенческие требования, границы задачи и критерии приёмки |
| [solution-design](../../skills/solution-design/SKILL.md) | Реальные варианты, компромиссы, выбранный подход и план проверки |
| [planning-and-decomposition](../../skills/planning-and-decomposition/SKILL.md) | Порядок небольших независимо проверяемых изменений |
| [minimal-change](../../skills/minimal-change/SKILL.md) | Контроль объёма diff и обоснование необходимых расширений |
| [root-cause-analysis](../../skills/root-cause-analysis/SKILL.md) | Проверяемые гипотезы, различающие эксперименты и устранение причины |
| [verification](../../skills/verification/SKILL.md) | Критерии приёмки, фактические проверки, итоговый diff и честный статус |
| [production-readiness](../../skills/production-readiness/SKILL.md) | Блокеры релиза, эксплуатационные доказательства, выкатка и восстановление |

### Реализация и проверка кода

| Навык | Для чего применять |
|---|---|
| [software-development](../../skills/software-development/SKILL.md) | Идиоматичный код, владение ресурсами, ошибки и ограничения роста |
| [ui-ux-design](../../skills/ui-ux-design/SKILL.md) | Пользовательские сценарии, состояния, дизайн-система и адаптивный интерфейс |
| [software-architecture](../../skills/software-architecture/SKILL.md) | Границы модулей и сервисов, доменные правила и направление зависимостей |
| [refactoring](../../skills/refactoring/SKILL.md) | Постепенное изменение структуры с сохранением поведения |
| [debugging-and-profiling](../../skills/debugging-and-profiling/SKILL.md) | Воспроизведение, отладчик, трассировки, CPU, память и I/O |
| [testing](../../skills/testing/SKILL.md) | Unit, integration, contract, e2e, property и fuzz-проверки |
| [security-engineering](../../skills/security-engineering/SKILL.md) | Границы доверия, сценарии атак и обоснованные исправления |
| [code-review](../../skills/code-review/SKILL.md) | Конкретные замечания к diff с приоритетом, условиями и последствиями |
| [cleanup-simplification](../../skills/cleanup-simplification/SKILL.md) | Временный код, забытые вспомогательные функции и лишняя сложность |
| [legacy-code](../../skills/legacy-code/SKILL.md) | Характеризационные тесты, неявные контракты и постепенная замена |
| [compatibility](../../skills/compatibility/SKILL.md) | Платформы, клиенты, среды исполнения и работа старых/новых версий |
| [accessibility](../../skills/accessibility/SKILL.md) | Клавиатура, фокус, семантика, скринридеры, контраст и reflow |

### Контракты, состояние и производительность

| Навык | Для чего применять |
|---|---|
| [api-design](../../skills/api-design/SKILL.md) | REST/RPC/GraphQL, ошибки, идемпотентность и пагинация |
| [database-engineering](../../skills/database-engineering/SKILL.md) | Схемы, планы SQL, индексы, транзакции и изоляция |
| [distributed-systems](../../skills/distributed-systems/SKILL.md) | Частичные отказы, репликация, повторы и семантика доставки |
| [concurrency](../../skills/concurrency/SKILL.md) | Владение, гонки, deadlocks, жизненный цикл задач и порядок памяти |
| [performance-engineering](../../skills/performance-engineering/SKILL.md) | Репрезентативные замеры, узкие места, кэширование и эффект оптимизаций |
| [networking](../../skills/networking/SKILL.md) | DNS, транспорт, TLS, HTTP, прокси и балансировщики |
| [data-engineering](../../skills/data-engineering/SKILL.md) | Batch/stream-пайплайны, эволюция схем, повторная обработка и сверка |
| [migration-engineering](../../skills/migration-engineering/SKILL.md) | Expand/migrate/contract, возобновляемый backfill и восстановление |

### Эксплуатация и доставка

| Навык | Для чего применять |
|---|---|
| [observability](../../skills/observability/SKILL.md) | Логи, метрики, трассировки, SLI/SLO и полезные оповещения |
| [devops-infrastructure](../../skills/devops-infrastructure/SKILL.md) | Контейнеры, оркестрация, конфигурация хостов и IaC |
| [ci-cd](../../skills/ci-cd/SKILL.md) | Проверки в pipeline, воспроизводимые входы, артефакты и границы доверия |
| [release-engineering](../../skills/release-engineering/SKILL.md) | Версии, release notes, проверка артефактов и поэтапная выкатка |
| [dependency-management](../../skills/dependency-management/SKILL.md) | Версии, lockfiles, уязвимости, происхождение, SBOM и вопросы лицензий |
| [reliability-engineering](../../skills/reliability-engineering/SKILL.md) | Отказы, ограничение нагрузки, деградация, ёмкость и восстановление |
| [incident-response](../../skills/incident-response/SKILL.md) | Triage, стабилизация, проверка восстановления, хронология и дальнейшие действия |
| [documentation](../../skills/documentation/SKILL.md) | README, справочники, ADR, архитектурные документы и runbooks |
| [git-scm](../../skills/git-scm/SKILL.md) | Коммиты, ветки, конфликты, bisect и запрошенное переписывание истории |
| [build-systems](../../skills/build-systems/SKILL.md) | Граф сборки, toolchains, генерируемые файлы и воспроизводимость |
| [cost-engineering](../../skills/cost-engineering/SKILL.md) | Источники затрат, стоимость единицы работы и подтверждённая экономия |
| [technical-research](../../skills/technical-research/SKILL.md) | Первичные источники, версии, сравнение решений и ограниченные PoC |

## Загрузка и проверка

В рабочей копии проекта используется существующая настройка встроенных навыков:

```sh
ROOK_BUILTIN_SKILLS="$PWD/skills" cargo run -p rook-cli -- skills ls
ROOK_BUILTIN_SKILLS="$PWD/skills" cargo run -p rook-cli -- skills show problem-analysis
cargo test -p rook-skills --test builtin
```

Упаковка релиза уже включает `skills/`; отдельная регистрация не нужна. Как и
любой встроенный навык, файл может быть переопределён одноимённым пользовательским
или проектным skill. Дополнительно установленные навыки могут превысить
`agent.max_skill_cards`; ограничения описаны в разделе
[постепенное раскрытие](skills.md#постепенное-раскрытие).

Новые навыки используют переносимые обязательные поля `name` и `description`.
Они не требуют определённых внешних инструментов: процедура использует доступные
средства целевого проекта. Обязательный конкретный инструмент и его версия
должны объявляться в `requires` специализированного навыка.

Тесты встроенных навыков проверяют обнаружение, применимость, формулировки условий
и бюджеты карточек/тел. Это структурная проверка: она не доказывает, насколько
надёжно конкретная модель выбирает и выполняет инструкции в реальных задачах.
