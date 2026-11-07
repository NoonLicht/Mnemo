# Speed report / Отчёт о скорости

## Русский

**Где:** Настройки → Состояние → «Отчёт о скорости». Кнопка «Скопировать» кладёт в буфер Markdown, который можно вставить в GitHub.

**Что в нём:**
- время этапов последнего сканирования: обход папок, метаданные, извлечение текста и OCR, фиксация индекса;
- по каждой папке и диску: тип диска (NVMe SSD, SATA SSD, SATA HDD), модель, число записей, скорость обхода, сколько файлов прочитано, объём и время чтения, число изображений и время их обработки (OCR, если включён);
- задержки поиска (p50, p95, максимум) по этапам: загрузка индекса, разбор запроса, поиск по именам, полнотекст (tantivy), векторы, объединение результатов, ранжирование, фрагменты.

**Приватность:** данные хранятся только на этом компьютере в `%APPDATA%\mnemo\perf\` (JSON-строки). Текст запросов не записывается, только его длина. Кнопка «Очистить» стирает историю.

**Ограничения:**
- Тип диска определяется через Windows (`IOCTL_STORAGE_QUERY_PROPERTY`). На Linux и macOS он показывается как `unknown`.
- «Время чтения» суммируется по всем потокам, поэтому может быть больше времени этапа.
- Время по папкам зависит от общей нагрузки: потоки читают файлы с разных дисков одновременно.

**Замер без приложения:** `cargo run --release --example perf_probe -- C:\Папка D:\ F:\` строит временный индекс (настоящий не трогается) и печатает тот же отчёт.

## English

**Where:** Settings → Health → "Speed report". "Copy" puts Markdown on the clipboard, ready to paste into GitHub.

**What it contains:**
- stage times of the last scan: walk, metadata, text extraction and OCR, index commit;
- per folder and drive: drive type (NVMe SSD, SATA SSD, SATA HDD), model, entries found, walk speed, files read, bytes and read time, image count and image processing time (OCR when enabled);
- search latency (p50, p95, max) per stage: index load, query parsing, filename match, full text (tantivy), vectors, fusion, ranking, snippets.

**Privacy:** data stays on this computer in `%APPDATA%\mnemo\perf\` (JSON lines). Query text is never recorded, only its length. "Clear" deletes the history.

**Limits:**
- The drive type comes from Windows (`IOCTL_STORAGE_QUERY_PROPERTY`). On Linux and macOS it shows `unknown`.
- "Read time" is summed over all worker threads, so it can exceed the stage's wall-clock time.
- Per-folder times depend on overall load: workers read from several drives at once.

**Measuring without the app:** `cargo run --release --example perf_probe -- C:\Folder D:\ F:\` builds a throwaway index (the real one is untouched) and prints the same report.
