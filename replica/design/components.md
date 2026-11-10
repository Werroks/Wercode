# Components: Wercode

Токены: `tokens.json` (генерируется `build_tokens.py`), темы: `themes/*.toml`.
Основа: компоненты `gpui-component`; в коде — только роли (`bg`, `surface`, `text-muted`…), никаких hex.
Иконки: Lucide (ISC), 16 px, толщина линии 1.5. Свои рисуем только для логотипа.
Шрифты: интерфейс — системный; терминал и редактор — моноширинный из настроек (по умолчанию Cascadia Mono, дальше Consolas / Menlo / DejaVu Sans Mono).
Тексты интерфейса — свои, на английском (публичный репозиторий), короткие, без точек в подписях кнопок.

## Схемы

| id | вид | характер |
| --- | --- | --- |
| black | тёмная, по умолчанию | чистый чёрный фон, нейтральные серые, синий акцент |
| graphite | тёмная | мягкий серо-графитовый фон, меньше контраста для долгой работы |
| midnight | тёмная | глубокий синий фон, сиреневый акцент |
| forest | тёмная | тёмно-зелёный фон, зелёный акцент |
| paper | светлая | тёплый бумажный фон |

Каждая схема задаёт сразу интерфейс, 16 цветов терминала и подсветку редактора. Проверка: `contrast.py tokens.json` — 210 пар, 0 провалов AA.
Акцентный цвет временный до `/replica-brand`.

## Раскладка окна

```
titlebar 36      [проект ▾]                                   [– □ ×]
tabbar 34        [pwsh ×] [main.rs ● ×] [+]        │ [files] [git] [search] [settings]   panel-header 36
                                                   │
                 центр (вкладки и сплиты)          │ правая панель 300 (200 … 50% окна)
                 отступ терминала 8                │ строка дерева 24
```

- Минимальный размер окна 640×400. При ширине меньше 720 панель сворачивается сама, кнопки остаются.
- Сплиттер: линия 1 px цвета `border`, зона захвата 6 px, при наведении и перетаскивании — `accent`. Двойной клик возвращает ширину по умолчанию.
- Кнопок в шапке панели четыре: Files — вид по умолчанию, плюс три из задумки (Git, Search, Settings). Если нужно ровно три — Files становится состоянием «ничего не выбрано», повторный клик по активной кнопке возвращает к дереву.
- Тени не используем, глубина — через `surface` / `surface-2` и границы. Анимации только для наведения (100 мс) и появления оверлеев (160 мс); в терминале анимаций нет.

## Компоненты

```
TerminalView
  variants  одиночный, в сплите
  states    focused (курсор сплошной, мигает по настройке), unfocused (курсор контуром),
            selecting, scrolled-back (метка «↓» внизу справа), exited (строка «[process exited: код]»,
            Enter перезапускает), bell (короткая вспышка заголовка вкладки)
  tokens    bg terminal.background, fg terminal.foreground, ansi[16], selection, mono/14, padding 8
  input     всё, кроме Ctrl+Shift+* и Alt+*, уходит в шелл; правый клик: копировать выделение, иначе вставить
  a11y      фокус виден по курсору и подсветке активной вкладки; размер шрифта Ctrl+= / Ctrl+- / Ctrl+0
  used on   W01

TabBar / Tab
  variants  terminal (иконка terminal + заголовок процесса), editor (иконка файла + имя), diff
  states    active (text, нижняя линия 2 px accent), inactive (text-muted), hover (surface-2),
            dirty (точка ● вместо ×), dragging, overflow (прокрутка колесом, без переноса)
  sizes     высота 34, ширина 120–220, заголовок обрезается с многоточием посередине
  actions   клик — активировать; средняя кнопка / × — закрыть; перетаскивание — порядок и перенос в сплит;
            [+] — новый терминал; контекстное меню: Close, Close Others, Split Right, Split Down
  a11y      Ctrl+Tab / Ctrl+Shift+Tab, Alt+1..9
  used on   W01

IconButton
  variants  panel switch (toggle), toolbar, tab close
  sizes     28×28, иконка 16, radius sm
  states    default (text-muted), hover (text, surface-2), active/toggled (text, surface-2, линия accent),
            focus-visible (кольцо 2 px accent), disabled (40% непрозрачности)
  a11y      подсказка с названием и клавишей через 500 мс; доступна с клавиатуры
  used on   W01, W02, W03

Tree
  variants  files, changes (git), search results (файл → совпадения)
  row       высота 24, отступ уровня 12, шеврон 12, иконка 16, имя, справа метка статуса
  states    collapsed, expanded, selected (surface-2 + text), focused (кольцо внутри строки), hover (surface),
            loading (спиннер вместо шеврона), ignored (text-muted, 60%), renaming (inline Input),
            git: M warning, A/U success, D danger, конфликт danger + «!»; папка наследует «старший» статус
  empty     «Empty folder» / «No changes» / «No results»
  actions   клик — выбрать и открыть; двойной клик на папке не нужен (одинарный раскрывает);
            стрелки, Enter, F2, Del; Ctrl+Enter или перетаскивание в терминал — вставить путь
  perf      виртуальный список, дети грузятся при раскрытии
  used on   W02, W03, поиск

Splitter
  см. «Раскладка окна». Курсор col-resize / row-resize. Используется между центром и панелью и между сплитами.

ContextMenu / Menu
  states    open, item hover (surface-2), item disabled, разделитель, клавиша справа (text-muted)
  tokens    bg surface, border, radius md, строка 28, текст sm
  a11y      стрелки, Enter, Esc; фокус возвращается на вызвавший элемент
  used on   дерево, вкладки, git-список, выбор ветки, список недавних

Command (палитра и быстрое открытие)
  layout    оверлей по центру сверху, ширина 560, до 12 строк, затемнения фона нет
  states    empty (подсказка), results (совпавшие буквы — accent), no matches («No matching commands/files»)
  row       название слева, клавиша или путь справа (text-muted)
  a11y      стрелки, Enter, Esc; фокус сразу в поле
  used on   W05

Input / Textarea
  variants  однострочный (поиск, rename), многострочный (commit message, 3–8 строк)
  states    default (border-input), focus (кольцо accent), error (danger + текст под полем), disabled
  sizes     высота 28, текст base, radius sm
  used on   W03, W04, W05, поиск

Button
  variants  primary (Commit), secondary, ghost, danger (Discard, Delete)
  sizes     sm 24, md 28
  states    default, hover, active, focus-visible (кольцо accent), disabled, loading (спиннер, подпись остаётся)
  tokens    primary: bg accent, text on-accent; danger: текст danger, при hover фон danger 15%
  used on   W03, W04, W07, диалоги

Editor
  base      Editor из gpui-component: номера строк, подсветка, сворачивание
  states    clean, dirty (● на вкладке), read-only (бинарный/слишком большой — заглушка с причиной),
            changed-on-disk (полоса сверху: «File changed on disk» [Reload] [Keep mine]),
            deleted-on-disk (полоса: «File was deleted» [Save anyway] [Close])
  tokens    bg ui.bg, текст syntax.*, номера строк text-muted, текущая строка surface, selection
  find      Ctrl+F — строка поиска сверху справа, Ctrl+H — с заменой
  used on   W06

DiffView
  base      Diff из gpui-component, unified
  states    added (success, фон 12%), removed (danger, фон 12%), context, binary («Binary file changed»),
            слишком большой («Diff is too large» [Open file])
  used on   W06

GitPanel (составной)
  layout    ветка и ahead/behind сверху (клик — меню веток) · Textarea сообщения · [Commit] ·
            секции Staged / Changes (Tree, variant changes) · внизу [Fetch] [Pull] [Push]
  states    no repo («Not a git repository» [Initialize]), clean («No changes»), changes, conflict,
            busy (кнопки disabled, спиннер), error (Toast с текстом git)
  row       при наведении справа: + (stage) / − (unstage) / ↶ (discard, с подтверждением)
  used on   W03

SearchPanel (составной)
  layout    Input запроса · переключатели Aa (регистр), .* (regex), ab (слово целиком) · фильтр путей ·
            счётчик «N results in M files» · Tree (variant search results)
  states    idle, searching (счётчик растёт, кнопка Stop), results, no results, too many (обрезано на 10 000)
  used on   панель поиска

Settings (составной)
  base      Settings из gpui-component; секции: Appearance (Theme, Font, Size), Terminal (Shell, Scrollback,
            Cursor, Copy on select), Editor (Tab size, Word wrap, Autosave), Keys, About
  theme     список схем с цветными образцами (bg, text, accent, 6 ANSI); применяется при наведении-выборе сразу
  states    значение по умолчанию / изменено (точка accent, кнопка сброса)
  used on   W04

TitleBar
  layout    слева кнопка проекта «имя ▾» (меню: недавние, Open Folder…), справа системные кнопки окна
  states    нет проекта («Open Folder…»), проект открыт; путь целиком — в подсказке
  used on   W01, W07

Welcome
  layout    по центру: название, [Open Folder] primary, список недавних (до 8), подсказки клавиш
  states    первый запуск (списка нет), есть недавние, недавний путь не существует (зачёркнут, × убрать)
  used on   W07

Dialog (подтверждение)
  uses      закрытие терминала с процессом, несохранённый файл, Discard, Delete
  layout    заголовок-вопрос, одна строка последствий, кнопки справа; опасная — danger; Esc = Cancel
  a11y      фокус по умолчанию на безопасной кнопке

Toast
  variants  info, error (danger слева полосой)
  states    появление снизу справа, авто-скрытие 5 с (ошибки — до закрытия), кнопка действия
  used on   все
```
