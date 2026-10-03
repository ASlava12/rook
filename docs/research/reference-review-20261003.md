# Другие агенты: обзор кандидатов для Rook, 3 октября 2026

## Объём и метод

Rook на начало обзора: `f003b53af8ad2c9e9fca6eab3f16bdbe7511cd6b`.
Учтены [архитектура](../architecture.md), [прошлый перенос](reference-adoption-20260929.md)
и [завершённый перенос Pi](pi-adoption-completion-20261003.md).

Проверены восемь других агентов и ACP: закреплённые деревья, свежие upstream
ветки, выбранные изменения реализации и тестов, официальные release notes.
В каждом подмодуле выполнен `git fetch --no-tags --depth=60 origin BRANCH`;
все девять fetch завершились с кодом 0. Checkout и gitlink остались прежними.
Новый код читался через `git show FETCH_HEAD:path` и `git grep ... FETCH_HEAD`.
Это позволяет воспроизвести обзор по постоянным ссылкам ниже, не меняя снимки.

Важная граница слова «новое»: снимки в `references` датированы 11–15 сентября,
а предыдущий обзор — 29 сентября. Сравнение деревьев показывает изменения
относительно этих снимков; некоторые уже описаны в прошлом обзоре или перенесены
в Rook. Выборка последних 60 коммитов не является полной хронологией изменения
ветки. Даты автора и коммитера различаются; release notes не доказывают попадание
каждого изменения текущего main в выпущенный бинарник.

Это исследование по исходникам. Чужие агенты и их тесты не запускались,
быстродействие и корректность кандидатов в Rook не измерялись. Приоритеты и
оценка сложности ниже — выводы для нашего проекта.

| Reference | Закреплённый снимок | Просмотренная upstream ветка и commit |
|---|---|---|
| ACP | `c849ac2` | main: [856cc831d56b86e5cd2dcf2c22082b618fb33ae6](https://github.com/agentclientprotocol/agent-client-protocol/tree/856cc831d56b86e5cd2dcf2c22082b618fb33ae6) |
| Codex | `c4017a8` | main: [6326163b9abd7802c0e57be4e326e5f898bbba75](https://github.com/openai/codex/tree/6326163b9abd7802c0e57be4e326e5f898bbba75) |
| Goose | `50666ae` | main: [591edd47cf2cfea4957d720c607cf2a4def8673d](https://github.com/aaif-goose/goose/tree/591edd47cf2cfea4957d720c607cf2a4def8673d) |
| OpenCode | `95daf90` | dev: [907b3bc518fa48e90e8ec24dd327d13eee71c36c](https://github.com/anomalyco/opencode/tree/907b3bc518fa48e90e8ec24dd327d13eee71c36c) |
| Cline | `cfe9cad` | main: [39ff2359f7e08231281539696e48a166ce49270c](https://github.com/cline/cline/tree/39ff2359f7e08231281539696e48a166ce49270c) |
| Hermes | `f364c19` | main: [bd0affe5e5f723579df8902852f5d0c47795f355](https://github.com/NousResearch/hermes-agent/tree/bd0affe5e5f723579df8902852f5d0c47795f355) |
| OpenHands | `de5a79b` | main: [8b0be7d5181db68be05261dbeafb580f7a5a4140](https://github.com/OpenHands/OpenHands/tree/8b0be7d5181db68be05261dbeafb580f7a5a4140) |
| OpenClaw | `bcd0534` | main: [9a8053f7ad252dbdfcd9de2cecde9d129e892b83](https://github.com/openclaw/openclaw/tree/9a8053f7ad252dbdfcd9de2cecde9d129e892b83) |
| OpenResearch | `325eb50` | main: [f4cec9f010a64fccf51cd4653ba548df2e5fb648](https://github.com/alphaXiv/OpenResearch/tree/f4cec9f010a64fccf51cd4653ba548df2e5fb648) |

Pi остаётся на `ee602414c703be8da722ec56de7f2399e62581ac`: его перенос уже
имеет отдельный завершённый трекер и результаты реальных сравнений.

## Рекомендуемый порядок

| Приоритет | Кандидат | Польза для Rook | Сложность |
|---|---|---|---|
| P1 | Повторы в потоке и циклы действий без прогресса — Hermes/OpenClaw | Раньше прерывать деградацию модели; показывать конкретную причину и сохранять пригодное состояние для продолжения | Средняя; особенно важны ложные срабатывания |
| P1 | `Retry-After` из потоковых ошибок Responses — Codex | Соблюдать паузу сервера после HTTP 200, завершившегося `response.failed` | Малая |
| P1 | Объединение перерисовок браузера — OpenHands | Длинный ответ не требует полного Markdown-парсинга на каждый сетевой фрагмент | Малая–средняя |
| P2 | Подагенты и компактация как отдельные события ACP | В IDE видны дочерние задачи и границы сжатия, с точной принадлежностью событий | Средняя–высокая; preview/unstable |
| P2 | Обновляемый справочник моделей и цен — Goose/Hermes | Меньше ручной настройки известных облачных моделей; источник и давность оценки видны | Средняя |
| P2 | Восстановление исчезнувшего delegated worktree — OpenResearch | Диагностика и восстанавливаемый checkout после внешнего удаления каталога | Средняя |
| Audit | Цена доставки/сохранения прогресса — Cline/OpenClaw | Проверить отсутствие роста записей и блокировки демона от живых обновлений | Средняя; это проверка существующего механизма |

Все строки — предложения, не новый активный implementation tracker.

## 1. Защита от зацикливания: Hermes и OpenClaw

У Hermes [repetition_guard.py](https://github.com/NousResearch/hermes-agent/blob/bd0affe5e5f723579df8902852f5d0c47795f355/agent/repetition_guard.py)
расширился относительно закреплённого снимка: проверяет не только продолжение
ответа, упёршегося в лимит, но и завершённый текст/живой поток. Детектор использует
длинные повторяющиеся фрагменты, отдельный высокий порог для готового ответа и
ограниченное хвостовое окно для потока. При сохранении прерванного ответа есть
нейтральный маркер: повторяющийся мусор не должен снова посеять тот же цикл в
следующем запросе. Простое совпадение общих префиксов разных строк недостаточно.

У OpenClaw [tool-loop-detection.ts](https://github.com/openclaw/openclaw/blob/9a8053f7ad252dbdfcd9de2cecde9d129e892b83/src/agents/tool-loop-detection.ts)
различает повтор вызова, чередование двух вызовов, polling без прогресса,
перебор аргументов и общий аварийный порог. История ограничена, аргументы и
результаты хешируются; есть область конкретного run и отдельные предупреждения.
Часть механизма существовала до свежего diff — полезен сам подход и его
текущие уточнения, а не утверждение, что всю защиту добавили на этой неделе.

В Rook уже есть [guard одинаковых вызовов/ответов](../../crates/rook-core/src/agent.rs)
и остановка managed goal после повторных ходов без действий. Но guard создаётся
для каждого turn, а `run_command` очищает его историю. В
[receive](../../crates/rook-core/src/agent/stream.rs) отдельного детектора
повторяющегося текста нет. Это установленное различие механизмов, не
воспроизведённый дефект и не новое доказательство причины прошлой сессии.

**Перенос:** сначала ограниченный детектор деградации потока и понятный stop
reason; затем отдельная защита от циклов действий. Связать предупреждение с
run/goal generation, не хранить полные результаты ради детектора. Команды
ожидания должны учитывать живость процесса; одинаковые запрошенные таблицы,
шаблонный код и проверки после правки должны оставаться допустимыми. При
прерывании сохранить исходный текст как диагностические данные, а политику
его replay определить явно, без разрушения tool bindings и opaque state.

**Проверить:** повторы через границы delta; Unicode; нормальные похожие строки;
новый пользовательский запрос; компактацию/restart; действительно работающий
polling; явные счётчики использования и interrupted physical attempt. Детектор
не решает за пользователя вопросы разрешений и не подтверждает выполнение цели.

## 2. Задержка повторного запроса: Codex

[Коммит 44dd77b](https://github.com/openai/codex/commit/44dd77b71e88c78295736bffd3dc3b684c13be6d)
читает `Retry-After` из `response.error.headers` потокового `response.failed`.
Это иной путь, чем обычный HTTP 429: ответ уже начался с HTTP 200. Валидная
задержка заголовка имеет приоритет над извлечённой из текста ошибки; терминальные
ошибки сохраняют свою классификацию.

В Rook [response_error](../../crates/rook-llm/src/openai/responses.rs) возвращает
`retry_after: None`; HTTP-заголовки читаются в другом месте. Общий
[Retrying](../../crates/rook-llm/src/retry.rs) уже умеет ждать server delay.
Поэтому это небольшой конкретный перенос через существующий `LlmError::Status`.
Jitter можно исследовать отдельно: сейчас экспоненциальные паузы детерминированы.

**Проверить:** SSE 200 → failed/429 или 503 → следующий запрос после задержки;
невалидный, слишком большой или отрицательный header; приоритет источников;
отмена во время ожидания; неизменность поведения auth/invalid-request ошибок.
Сохранение physical attempts и показ ожидания должны работать по существующим
путям. Начинать предлагаю с этого переноса: мало затронутых слоёв и ясная проверка.

## 3. Перерисовка и идентичность потоковых событий: OpenHands

[Изменение 1ec8661](https://github.com/OpenHands/OpenHands/commit/1ec8661)
заменило согласование текста по совпадению строк на идентификацию streaming
slots событиями. [session-seq-cursor.ts](https://github.com/OpenHands/OpenHands/blob/8b0be7d5181db68be05261dbeafb580f7a5a4140/src/utils/session-seq-cursor.ts)
учитывает пропуски последовательности, а
[use-streamed-text.ts](https://github.com/OpenHands/OpenHands/blob/8b0be7d5181db68be05261dbeafb580f7a5a4140/src/hooks/use-streamed-text.ts)
распределяет показ текста по кадрам. Это полезные способы разделить получение
данных, идентичность записи и отображение.

Rook [chat.js](../../web/dist/chat.js) явно перерисовывает текущий Markdown-блок
из всего текста на каждую delta. Это кандидат на объединение обновлений по
`requestAnimationFrame`, со сбросом последнего фрагмента на Done/Error/Stop.
Нагрузка на длинном fenced ответе пока не измерена. Искусственную задержку
«печатания» добавлять не требуется.

Rook уже имеет bounded replay/snapshot и durable receipt IDs. Заимствовать
чужой sequence cursor целиком не предлагается: порядок и смысл наших событий
другие. Полезно проверить поздние события после смены ветки и reconnect,
сохраняя принадлежность session/turn вместо сопоставления по тексту.

**Проверить:** фактическое число Markdown-парсингов на длинном ответе; сохранение
полного финального текста; split fences; Stop/ошибку в ожидающем кадре; позднюю
delta старой ветки; draft/questions/queue и отсутствие дополнительного роста
памяти. Подтверждать ускорение после измерения.

## 4. ACP: видимые подагенты, компактация и ошибки

В текущем [subagents RFD](https://github.com/agentclientprotocol/agent-client-protocol/blob/856cc831d56b86e5cd2dcf2c22082b618fb33ae6/docs/rfds/subagents.mdx)
у ребёнка собственный session ID, связь с родителем обновляется через
`subagent_update`, сообщения адресуются сессиям. В v1 клиент должен объявить
`subagents`; без этого агент не отправляет новые updates. Это ограниченная
дочерняя сессия, не автоматически доступная для произвольного user prompt.

[Compaction RFD](https://github.com/agentclientprotocol/agent-client-protocol/blob/856cc831d56b86e5cd2dcf2c22082b618fb33ae6/docs/rfds/session-compaction.mdx)
описывает lifecycle с `compactionId`, состоянием и опциональным отображаемым
summary. В changelog subagents обозначены unstable, а session compaction
переведена в preview. [59172ba](https://github.com/agentclientprotocol/agent-client-protocol/commit/59172bafdf4bb283e9f2b5baf8aa7724c9f2c65b)
также различает ошибку после вставки prompt и ошибку до принятия.

В Rook [ACP adapter](../../crates/rook-acp/src/lib.rs) уже отдаёт `usage_update`,
но `Delegating` представляет как thought chunk в родительской сессии. Можно
вывести существующие child IDs/связи и границы компактации структурированно,
с capability negotiation и прежним fallback. Это предложение для следующего
этапа, а не основание объявлять draft частью обязательного stable протокола.

**Проверить:** старый клиент; отказ клиента от capability; два одновременных
ребёнка; вложенную связь; восстановление/повтор updates; отсутствие чужого
opaque state в UI summary; допустимость cancel конкретного ребёнка в runtime.

## 5. Справочник моделей/цен: Goose и Hermes

Goose [4dea9b4](https://github.com/aaif-goose/goose/commit/4dea9b483efbd2541d43500b8ed3c044c65e6d2f)
добавляет live models.dev с кэшем и bundled fallback. Hermes
[ddc8c2dc](https://github.com/NousResearch/hermes-agent/commit/ddc8c2dcb639d8482f67f5d0f16b889008e5d65e)
проверяет upstream identity для цен custom endpoint.

Rook уже имеет scoped live/offline catalog, наблюдения capabilities и сохранённые
rate estimates. Пробел — удобное заполнение справочных облачных цен. Возможен
явно обновляемый bounded справочник для модели с известной identity: источник,
время наблюдения, input/output/cache rates и подтверждение оператора при
применении. Прайс известного облачного поставщика не определяет цену модели с
тем же именем в LM Studio, прокси или произвольном custom endpoint. Каталог
не должен автоматически заменять проверенные live capabilities или вручную
заданные значения. Старые receipts остаются замороженными.

**Проверить:** admission до копирования JSON; schema/count/byte bounds; offline
без сетевых/credential-helper вызовов; испорченный кэш; неоднозначный alias;
смену endpoint/account; цену cache tokens; неизвестную цену локального запуска.

## 6. Worktree recovery: OpenResearch

[1af3d02](https://github.com/alphaXiv/OpenResearch/commit/1af3d0250348432bbcbecca17485a2a72cdd7001)
и предшествующее восстановление потерянного checkout используют оставшуюся
git-регистрацию, сохраняют последнюю ветку/commit и учитывают вновь созданные
файлы в каталоге. Важно читать реализацию: ограничение относится к пути
`ensure_worktree_from`; отдельная явная функция удаления всё ещё использует
рекурсивное удаление/force и не является образцом для копирования.

Rook [worktrees.rs](../../crates/rook-core/src/worktrees.rs) уже отказывает
удалять running/interrupted tree, берёт lease и требует `discard=true` для
force. Новая ценность — read-only диагностика отсутствующего каталога и явное
восстановление зарегистрированного checkout без потери вновь созданных файлов.
Ни last checkout, ни git-регистрация не доказывают восстановление незакоммиченных
изменений. Для них нужен существующий capture/checkpoint, если он сохранён.

**Проверить:** missing directory с живой регистрацией; занятый lease;
пересозданный каталог; symlink/другой repository; dirty/untracked файлы;
Windows long paths и допустимые корни. Автоматический `git worktree prune`
или массовый force cleanup не входят в предложение.

## 7. Что взять как регрессионную проверку, а не новую функцию

Cline [5349bed](https://github.com/cline/cline/commit/5349bed08f36b53b158205ddefee7ebbfd61d12a)
разделяет transient stream/heartbeat и durable состояние команды, сохраняет
только изменения, повторяет неуспешные записи и ограничивает историю.
Это конкретный урок о цене целого snapshot на каждый token. Rook уже использует
append-only transcript и bounded frozen child accounting. Полезна нагрузочная
проверка записи/очереди прогресса делегирования, а не новый параллельный team store.
Critical admission/ending receipts должны оставаться durable; batching живого
отображения не даёт права откладывать их фиксацию перед внешним действием.

OpenClaw [0c96fd043](https://github.com/openclaw/openclaw/commit/0c96fd043ab36303f1e2098766f3e6d2a26ab8cf)
устраняет холодную пересборку runtime/plugins для rooted background runs.
Rook уже строит MCP/LSP/approval services на уровне frontend. Стоит проверить
reuse и отзыв старой generation в delegated worktree/расписании: разные execution
roots не должны смешивать project trust и контекст, а тяжёлая подготовка не
должна удерживать engine lock или блокировать текущую доставку.

Codex [86a54b0](https://github.com/openai/codex/commit/86a54b051c08f34f373c507ae16a91915ab08700)
проверяет четыре состояния вокруг remote `/compact`: запрос до него, запрос
сжатия, replacement history до следующего turn и первый последующий запрос.
Rook уже тестирует compaction/reopen/opaque batches; полезно добавить именно
сценарий долгой задачи с принятой корректировкой, неотвеченным вопросом и
запретом повторять уже отвергнутый подход. Это тест сохранения конкретных
инвариантов, а не требование копировать vendor-specific encrypted checkpoints.

## 8. Остальные наблюдения и решения

- **Goose:** новый HTML export, цитирование, модели, vision и права — значительная
  часть пользы уже покрыта Rook. MCP redirect guard полезен для сверки:
  [наш HTTP transport](../../crates/rook-mcp/src/http.rs) уже отключает redirects.
  MCP Apps PiP требует другого desktop/container UI; сейчас невысокий приоритет.
- **OpenCode:** свежий core change
  [e9f8a21](https://github.com/anomalyco/opencode/commit/e9f8a210b9e2b1e13d375b84906069886eb3b767)
  — namespaced session/parent headers. Для Rook это возможная opt-in корреляция
  с proxy, не причина отправлять session IDs всем внешним endpoints по умолчанию.
  [b471c2b](https://github.com/anomalyco/opencode/commit/b471c2b4495747353af768fbf2e0790c9d820ce2)
  проверяет уже завершившийся browser launcher, включая Windows/WSL; полезный
  сценарий для OAuth opener. Остальная просмотренная свежая выборка содержит
  много stats/site/provider-plan работы с низкой применимостью к Rook.
- **Hermes:** исправления follow-up ownership и multi-connection attribution
  полезны как сценарии; в Rook generation/session identity уже проверяется
  текущими race/restart тестами. Desktop, социальные каналы и browser profiles
  не требуют переноса ради этого обзора.
- **OpenHands:** голосовой ввод и templates automation — возможные UX дополнения;
  пользы для надёжности многодневной работы сейчас меньше. Classifier при старте
  разговора не обосновывает автоматическую маршрутизацию: наше реальное
  сравнение [уже сохранило провалы качества](pi-phase-comparison-20261003.md).
- **OpenResearch:** восстановление SSH supervision, пояснение неуспешного skill
  screening/backoff, model/experiment telemetry интересны для будущих remote
  compute задач. У Rook пока другой runtime; перенести целый SSH/Slurm subsystem
  вместо отдельного требования не предлагается. MCP, usage attribution и
  существующая история skills уже имеют реализацию в Rook.

## Следующий конкретный блок

Начать с `Retry-After` в Responses SSE: один небольшой перенос с проверкой
реального HTTP stream и существующего retry wrapper. Затем детектор зацикленного
потока с контролируемыми повторяющимися и нормальными ответами; после него —
браузерное объединение кадров с измерением числа перерисовок. Новые функции ACP
и каталог цен требуют отдельного согласованного контракта.

Результат этого блока — обзор кандидатов с проверяемыми источниками. Код Rook,
форматы, конфигурация пользователя и закреплённые подмодули сохранены.

Перед коммитом проверены 12 локальных ссылок и существование 26 upstream
commit/file объектов по указанным SHA (код 0). Проверка whitespace staged diff
также входит в сохранение блока. Это проверка документа и источников;
новой проверки runtime/CI здесь не заявляется.
