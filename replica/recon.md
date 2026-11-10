# Recon map: Cursor (desktop: Windows / macOS / Linux)

Scope: только оболочка рабочего окна — терминал, дерево файлов, git-панель, настройки, палитра команд. Без AI-чата, агентов, облака, расширений, полноценного редактора.
For: публичный open-source продукт на Rust для тех, кто работает через терминал (Claude Code, Codex CLI, vim, lazygit и т. п.) и хочет минимальную оболочку вместо IDE.
Date: 2026-11-11

Метод: только публичные источники (доки, changelog, форум, обзоры). Аккаунт Cursor не использовался, бинарник и бандлы не читались. Скриншоты не снимались — папка `replica/screens/` пуста.

Условия Cursor (п. 1.5): запрещены reverse engineering / декомпиляция и использование сервиса для обучения конкурирующей модели. Сбор публичных UX-паттернов и собственная реализация с нуля под эти запреты не подпадают; в приложение Cursor ради этого проекта не лезем.

## Sources

| # | source | URL | notes |
| --- | --- | --- | --- |
| 1 | docs: Agents Window | https://cursor.com/docs/agent/agents-window | обзор, шорткаты Cmd+P / Cmd+Shift+F, список возможностей; раскладка панелей не описана |
| 2 | changelog 3.0 | https://cursor.com/changelog/3-0 | Agents Window, Design Mode, Agent Tabs |
| 3 | changelog 2.3 | https://cursor.com/changelog/2-3 | четыре раскладки: agent, editor, zen, browser (по выдаче поиска, страница не открывалась) |
| 4 | форум: раскладки | https://forum.cursor.com/t/sidebar-right-layout-switches-window-layout-to-agent-view-in-cursor-settings/164741 | ответ сотрудника: Editor layout — сайдбар слева, Agent layout — сайдбар справа |
| 5 | форум: «Return to clean interface?» | https://forum.cursor.com/t/return-to-clean-interface/159744 | жалобы на навязанный Agents Window, нет настройки вида по умолчанию |
| 6 | форум: пустой промежуток | https://forum.cursor.com/t/empty-editor-gap-between-sidebar-and-chat-panel-persists-across-restarts-cache-clears-and-updates/156669 | пустая область редактора между деревом и чатом |
| 7 | обзор DataCamp | https://www.datacamp.com/blog/cursor-3 | diffs view: пословный diff, stage, commit, PR; настройки; marketplace |
| 8 | Terms of Service | https://cursor.com/terms-of-service | п. 1.5, ограничения |
| 9 | цены (вторичный источник) | https://vibecodehq.pages.dev/blog/cursor-3-agents-window-unified-workspace | Hobby $0, Pro $20, Pro+ $60, Ultra $200 |
| 10 | Terax (конкурент) | https://github.com/crynta/Terax-AI | Tauri 2 + React + xterm.js, ~9.5k звёзд, Apache-2.0 |
| 11 | SEE (конкурент, заброшен) | https://github.com/Advait1306/see | GPUI + alacritty_terminal + git2 + ropey, только macOS |
| 12 | gpui-component | https://github.com/longbridge/gpui-component | dock, resizable panels, tabs, virtual lists, редактор; Windows/macOS/Linux |
| 13 | Zed 1.0 | https://zed.dev/blog/zed-1-0 | 29.04.2026, Windows вышел из preview — GPUI на Windows боевой |
| 14 | gpui-terminal | https://docs.rs/gpui-terminal | готовый терминальный компонент на alacritty_terminal |

Достоверность: раскладка классического окна Cursor (форк VS Code) известна хорошо, но в источниках выше подтверждена лишь частично — такие строки помечены «средняя».

## Core loop

Открыл папку — получил терминал, рядом видно файлы и состояние git; всё остальное делается командами в терминале.

## Screens

Экраны оригинала. Колонка «в клон» — берём ли в наш срез.

| ID | screen | route / how to reach | purpose | key components | states seen | в клон |
| --- | --- | --- | --- | --- | --- | --- |
| S01 | Окно Editor layout | запуск, `cursor . --classic` | классическая IDE: activity bar, сайдбар слева, редактор, нижняя панель, чат справа, статус-бар | title bar, activity bar, sidebar, tabs, panel, status bar | пустое (нет папки), папка открыта | нет (это и есть «бардак») |
| S02 | Окно Agent layout / Agents Window | Cmd+Shift+P → Open Agents Window; Settings → General → Layout | агенты слева, чат по центру, сайдбар с файлами справа | список агентов, чат, сайдбар справа, тайлы | пусто, один агент, сетка агентов | раскладка — да (центр + правый сайдбар), содержимое — нет |
| S03 | Explorer (дерево файлов) | сайдбар, Ctrl+Shift+E | навигация по проекту | дерево, иконки, git-метки, контекстное меню, inline-rename | пусто, загрузка, большое дерево, игнорируемые файлы | да |
| S04 | Терминал | Ctrl+` (нижняя панель) | шелл внутри окна | вкладки терминалов, сплит, выбор шелла, kill | один, несколько, процесс завершён | да, становится центром |
| S05 | Source Control / Diffs view | Ctrl+Shift+G; в Agents Window — отдельный diffs view | изменения, stage, commit, push, PR | список изменённых файлов, поле сообщения, кнопка commit, diff с пословной подсветкой | нет репо, чисто, есть изменения, конфликт | да (без PR в MVP) |
| S06 | Командная палитра | Ctrl+Shift+P | любое действие с клавиатуры | поле ввода, нечёткий список | пусто, результаты, нет совпадений | да |
| S07 | Быстрое открытие файла | Ctrl+P | перейти к файлу | нечёткий поиск | то же | да |
| S08 | Поиск по проекту | Ctrl+Shift+F | grep по файлам | поле, фильтры, список совпадений | пусто, результаты, нет совпадений | could |
| S09 | Настройки | шестерёнка, Ctrl+Shift+J / Ctrl+, | General (в т. ч. Layout), Models, MCP, Secrets и др. | вкладки, переключатели, поля | — | да, сильно урезанные |
| S10 | Редактор / просмотр файла | клик по файлу | правка кода, LSP | вкладки, сплиты, minimap | — | только просмотр (read-only превью), правка — в `$EDITOR` в терминале |
| S11 | AI-чат / Composer / Agent Tabs | Ctrl+L, Ctrl+I | чат с моделью, агент | поле ввода, история, diff-карточки | — | нет |
| S12 | Встроенный браузер + Design Mode | раскладка browser, Cmd+Shift+D | превью и аннотации UI | webview, DevTools | — | нет |
| S13 | Marketplace / расширения | сайдбар | плагины, MCP | список, установка | — | нет |
| S14 | Приветствие / открыть папку | запуск без папки | выбрать проект, недавние | кнопка Open Folder, список недавних | первый запуск, есть недавние | да |

### Экраны клона (итоговая раскладка)

```
┌──────────────────────────────────────────────┬───────────────────┐
│ вкладки терминалов  [+]                      │ [⚙] [git] [ ? ]   │  ← три кнопки
├──────────────────────────────────────────────┼───────────────────┤
│                                              │ ▾ src             │
│                                              │   ▸ ui            │
│              ТЕРМИНАЛ                        │     main.rs    M  │
│                                              │   Cargo.toml      │
│                                              │                   │
└──────────────────────────────────────────────┴───────────────────┘
```

| ID | экран | откуда |
| --- | --- | --- |
| W01 | Главное окно: терминал + правая панель | S02 + S04 |
| W02 | Правая панель: Файлы | S03 |
| W03 | Правая панель: Git | S05 |
| W04 | Правая панель или оверлей: Настройки | S09 |
| W05 | Палитра команд / быстрый файл | S06, S07 |
| W06 | Превью файла / diff (оверлей или сплит) | S10, S05 |
| W07 | Приветствие | S14 |

Решения пользователя от 2026-11-11 (заменяют противоречащие строки выше и ниже):
- Три кнопки: настройки, git, поиск по проекту (S08).
- Встроенный редактор нужен: клик по файлу открывает его во вкладке рядом с терминалами (правка, сохранение, подсветка, поиск в файле). LSP — после MVP, по желанию.
- В верхней полосе окна — имя проекта с выпадающим списком «недавние / открыть папку».

- Тема по умолчанию чёрная; в настройках — выбор из нескольких встроенных цветовых схем (свои палитры, не темы Cursor).
- Сплит терминала (рядом и друг под другом) — обязательная фича.
- Поведение терминала сверяем с открытым кодом VS Code (MIT, `src/vs/workbench/contrib/terminal`) — терминал Cursor унаследован оттуда. Установленный Cursor не разбираем (п. 1.5 его условий).

Счёт матрицы после решений: must 31, should 12, could 10, skip 9. Размер: уверенный M, ближе к L.

## Flows

```
F01 Открыть проект и начать работать
    W07 -> W01
    happy path clicks: 2 (Open Folder, выбрать папку); 0 при запуске `wercode .`
    edge: папки нет, нет прав, сетевой диск, огромный репозиторий

F02 Найти файл и что-то с ним сделать
    W01 -> W02 (или W05) -> действие
    happy path clicks: 1–2
    действия: превью, вставить путь в терминал, открыть в $EDITOR, показать в проводнике
    edge: бинарный файл, файл удалён, симлинк, .gitignore

F03 Посмотреть изменения и закоммитить
    W01 -> W03 -> W06 (diff) -> stage -> сообщение -> commit -> push
    happy path clicks: 5
    edge: не репозиторий, нет user.name, конфликт, push отклонён, нет доступа

F04 Несколько терминалов
    W01 -> новая вкладка / сплит -> переключение
    happy path clicks: 1 (Ctrl+Shift+T)
    edge: процесс не завершён при закрытии, шелл упал

F05 Сменить настройку
    W01 -> W04 -> правка -> применилось сразу
    happy path clicks: 3
    edge: битый конфиг-файл

F06 Вернуться к работе
    запуск -> восстановлены проект, вкладки, рабочие каталоги, ширина панели
    happy path clicks: 0
```

## Components

| component | variants | states | used on |
| --- | --- | --- | --- |
| Terminal view | вкладка, сплит | фокус, без фокуса, процесс завершён, выделение, прокрутка | W01 |
| Tab bar | терминалы | активная, hover, перетаскивание, переполнение | W01 |
| Icon button | три кнопки панели, [+], закрыть | default, hover, active, disabled | W01 |
| Tree | файлы, изменённые файлы | свёрнут, развёрнут, выбран, git-статус (M/A/D/U), ignored, rename, загрузка | W02, W03 |
| Splitter | вертикальный | default, hover, drag, свёрнута панель | W01 |
| Context menu | файл, папка, вкладка | открыто, пункт disabled | W02, W03, W01 |
| Fuzzy list (палитра) | команды, файлы | пусто, результаты, нет совпадений | W05 |
| Text input | однострочный, многострочный (commit message) | default, focus, error | W03, W04, W05 |
| Button | primary (Commit), secondary, ghost | default, hover, disabled, loading | W03, W04, W07 |
| Diff view | unified | added, removed, context, пословная подсветка, бинарный файл | W06 |
| File preview | текст с подсветкой, изображение | загрузка, слишком большой, бинарный | W06 |
| Toggle / Select / Number | настройки | default, focus | W04 |
| Toast | info, error | появление, авто-скрытие | все |
| Status line (опц.) | ветка, ahead/behind, cwd | — | W01 |

## Inferred data model

Сервера и БД нет: состояние на диске в конфиг-файлах.

```
Settings   font_family, font_size, theme, shell, shell_args, scrollback,
           cursor_style, panel_side (right|left), panel_width, keybindings
           хранение: ~/.config/wercode/settings.toml
           evidence: S09, собственное решение
           confidence: high

Workspace  root_path, last_opened_at, panel_visible, active_panel (files|git|…),
           expanded_dirs[], tabs[]
           хранение: state.json в каталоге данных приложения
           evidence: S14 (недавние), F06
           confidence: high

TerminalTab  id, title, cwd, shell, split_layout
           evidence: S04
           confidence: high

FileNode   path, kind (file|dir|symlink), git_status, ignored, children (лениво)
           evidence: S03
           confidence: high (в памяти, не хранится)

GitState   branch, upstream, ahead, behind, staged[], unstaged[], untracked[],
           conflicts[]
           evidence: S05
           confidence: high (в памяти, читается из репозитория)
```

Relationships: Settings 1 глобально; Workspace 1-n TerminalTab; Workspace 1-1 GitState; Workspace 1 дерево FileNode.

## Feature matrix

See `features.csv`. Must: 22, should: 14, could: 9, skip: 11.

## Out of scope (cannot or should not be cloned)

- AI-чат, агенты, Tab-автодополнение, облачные агенты, модели — сервис Cursor, а не оболочка; в терминале пользователь запускает свой CLI-агент.
- Marketplace расширений VS Code и сами расширения — чужая экосистема.
- Встроенный браузер и Design Mode.
- Полноценный редактор с LSP — это путь к тому самому «бардаку»; правка через `$EDITOR`.
- Название, логотип, иконки, тема и тексты Cursor — своё имя и свой вид.
- Аккаунты, команды, биллинг, синхронизация.

## Конкуренты

| проект | стек | что есть | вывод |
| --- | --- | --- | --- |
| Terax | Tauri 2, React, xterm.js | терминал, редактор, git, AI-панель, web-превью; ~9.5k звёзд | ближайший по идее, но уже оброс функциями и это webview, а не нативный Rust |
| SEE | GPUI, alacritty_terminal, git2 | терминалы, дерево, git-ревью | почти наш стек; заброшен, только macOS, лицензия закрытая — смотреть как на подтверждение осуществимости, код не брать |
| Zed | GPUI | полноценный редактор с терминалом | тяжелее и редактор-центричен |
| Warp | Rust | терминал с AI и аккаунтом | нет дерева файлов как основы, закрытый |
| Lune, termesh, Reef | TUI | IDE внутри терминала | другой жанр: живут внутри чужого терминала |

Ниша свободна: нативный, без AI-панели, «терминал + файлы + git», кроссплатформенный, с открытой лицензией.

## Стек (предложение для replica-architect)

| слой | выбор | почему | запасной вариант |
| --- | --- | --- | --- |
| UI | GPUI + gpui-component | нативный GPU-рендер, готовые dock / сплиттеры / вкладки / виртуальные списки; Windows боевой с Zed 1.0 | Tauri 2 + xterm.js (быстрее старт, но UI на TypeScript) |
| эмуляция терминала | alacritty_terminal | стандарт де-факто, используется в Zed, SEE, gpui-terminal | libghostty-vt (Rust-биндинги 0.2, поддержка Windows не подтверждена) |
| PTY | portable-pty | ConPTY на Windows, pty на Unix | — |
| git: чтение | gix (status, diff) | быстрый параллельный status | git2 |
| git: запись | вызов системного `git` (commit, push, pull) | push в gix не готов; авторизация и хуки работают как у пользователя | git2 |
| файлы | notify + ignore | слежение и .gitignore | — |
| подсветка превью | tree-sitter или syntect | только чтение | — |
| конфиг | serde + toml | — | — |
| нечёткий поиск | nucleo | — | — |

Проверить перед стартом: актуальное имя и версию крейта gpui-component, наличие в нём tree-компонента, состояние gpui-terminal на Windows.

## Size

Screens 7, flows 6, entities 5.

Hard parts:
1. Терминал как у «настоящих»: ConPTY на Windows, resize, IME и раскладки, лигатуры и fallback шрифтов, мышь, выделение, производительность при большом выводе.
2. GPUI: мало документации, API меняется, Windows-ветка моложе остальных.
3. Дерево и git-статус на больших репозиториях: ленивая загрузка, слежение за изменениями без подвисаний.

Size: M (несколько недель до пригодного MVP на одной ОС; три ОС и полировка терминала — ближе к L).
