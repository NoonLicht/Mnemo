# Mnemo i18n — Implementation Plan (Phase 1 + 2)

Concrete, file-by-file execution plan. Phase 1 = infra + config + selector. Phase 2 = core search UI in Chinese. Total: ~1 week.

Prerequisite: `react-i18next`, `i18next`, `i18next-browser-languagedetector` already installed (commit pending).

---

## Phase 1 — Infrastructure

### 1.1 Create locale directory skeleton

Create empty JSON files so imports resolve:

```
src/locales/en/{common,search,home,results,details,help,settings,setup,errors}.json
src/locales/zh/{same 9 files}
```

Each file: `{}` initially, filled in Phase 2.

### 1.2 i18n config — `src/i18n/config.ts`

```ts
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";
import resourcesToBackend from "i18next-resources-to-backend";

export const SUPPORTED_LANGUAGES = {
  en: "English",
  zh: "中文",
} as const;
export type SupportedLanguage = keyof typeof SUPPORTED_LANGUAGES;

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .use(
    resourcesToBackend((language: string, namespace: string) =>
      import(`../locales/${language}/${namespace}.json`)
    )
  )
  .init({
    fallbackLng: "en",
    supportedLngs: Object.keys(SUPPORTED_LANGUAGES),
    defaultNS: "common",
    ns: ["common"],
    interpolation: { escapeValue: false },
    detection: {
      order: ["localStorage", "navigator"],
      caches: ["localStorage"],
      lookupLocalStorage: "mnemo.language",
    },
    react: { useSuspense: true },
  });

export default i18n;
```

### 1.3 Type-safe keys — `src/i18n/types.ts`

```ts
import "i18next";
import common from "../locales/en/common.json";
import search from "../locales/en/search.json";
// ... import all 9 namespaces

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "common";
    resources: {
      common: typeof common;
      search: typeof search;
      home: typeof home;
      results: typeof results;
      details: typeof details;
      help: typeof help;
      settings: typeof settings;
      setup: typeof setup;
      errors: typeof errors;
    };
  }
}
```

### 1.4 Barrel — `src/i18n/index.ts`

```ts
export { default } from "./config";
export * from "./types";
export { SUPPORTED_LANGUAGES, type SupportedLanguage } from "./config";
```

### 1.5 Wire into app entry — `src/main.tsx`

Add `import "./i18n";` before `<App />` render. Wrap `<App />` in `<Suspense fallback={null}>`.

### 1.6 Rust config field — `src-tauri/src/config.rs`

Mirror the `theme` field. Add:

```rust
#[serde(default)]
pub language: Option<String>,
```

Plus `normalize_language()`:
```rust
pub fn normalize_language(v: &str) -> Option<String> {
    match v {
        "en" | "zh" => Some(v.to_string()),
        _ => None,
    }
}
```

And migration in the config load path: if `language` is `None` and a saved value exists in the legacy location, migrate; otherwise leave `None` (auto-detect). Add a unit test mirroring `normalize_theme` tests.

### 1.7 TS type — `src/types.ts`

Add `language?: string | null;` to `MnemoConfig` (next to `theme`).

### 1.8 Language hook — `src/hooks/useLanguage.ts`

Mirror `useTheme.ts`:
- Read `config.language`.
- If `None`/auto: detect via `navigator.language` (map `zh*` → `"zh"`, else `"en"`).
- Call `i18n.changeLanguage(lang)`.
- Persist selection: `localStorage["mnemo.language"]` + `save_config({ language })`.
- Expose `{ language, setLanguage, supportedLanguages }`.

### 1.9 Settings dropdown — `src/components/settings/general.tsx` (or wherever theme lives)

Add a `<select>` bound to `useLanguage()`, options from `SUPPORTED_LANGUAGES` plus an "Auto (system)" entry (`null`). Reuse existing select styling.

### 1.10 `index.html` lang binding

In `App.tsx` (or a small effect in `useLanguage`): `document.documentElement.lang = i18n.language`.

### 1.11 Verification (Phase 1)

- `bun run build` (tsc + vite) passes.
- App launches; switcher changes `i18n.language` in console; no runtime errors.
- Missing keys fall back to English silently.

---

## Phase 2 — Core search UI in Chinese

Strategy: extract strings into namespace JSON, replace literals with `t("key")`. Do components in this order (highest user visibility first).

### 2.1 `common.json` (en + zh)

Keys: `app.name`, `actions.close`, `actions.cancel`, `actions.save`, `actions.copy`, `actions.open`, `actions.reveal`, `units.files`, `units.folders`, `units.results`, `time.justNow`, `time.minutesAgo`, `time.hoursAgo`, `time.daysAgo`.

Chinese translations provided.

### 2.2 `search.json`

Keys: `placeholder`, `slashMenu.*` (commands: search, filter, help, settings, reindex, …), `contextMenu.*` (open, reveal, copyPath, copyName, details, preview, …), `filters.*`, `sort.*` (best, newest, oldest, largest, name), `modes.ranked`, `modes.exhaustive`, `empty.noResults`, `empty.noIndex`.

### 2.3 Components to wire (in order)

1. `src/components/SearchBar.tsx` — placeholder, result count ("{{count}} results"), aria labels. Replace `toLocaleString("en-US")` with locale-aware.
2. `src/components/EmptyState.tsx` — empty-state captions.
3. `src/components/HomeView.tsx` — recent queries, suggestions, "no recents" text.
4. `src/components/ResultControls.tsx` — sort/view toggle labels, tooltips.
5. `src/components/ResultItem.tsx` / `FileRow.tsx` — aria labels, "open", size units.
6. `src/components/FilterChips.tsx`, `CategoryChips.tsx` — chip labels.
7. `src/components/KeyHintStrip.tsx` — hint text.
8. `src/components/ContextMenu.tsx` — menu item labels.
9. `src/components/HelpDialog.tsx` — help text (large; translate headings + shortcuts).
10. `src/components/DetailsDialog.tsx` — field labels.
11. `src/App.tsx` — slash-menu commands, toasts, error captions, key hints.

For each: `const { t } = useTranslation("namespace");` then replace literals. Add keys to both `en` and `zh` JSON in the same commit.

### 2.4 `src/lib/format.ts`

Replace `"en-US"` with `i18n.language` (or accept a `locale` param). Use `Intl.RelativeTimeFormat` for "m ago" style strings where possible; otherwise keep translated templates.

### 2.5 Verification (Phase 2)

- `bun run build` passes (tsc catches missing keys).
- Manual: switch to zh → main search screen fully Chinese; no raw keys; no English leaking in translated components.
- Switch back to en → identical to before.
- Auto-detect: set OS language to Chinese → app opens in zh.

---

## Phase 3 — Heavy modals (follow-up PR)

- `settings.json` + `setup.json` namespaces.
- Wire `t()` through `SettingsModal.tsx` and `SetupModal.tsx`.
- Backend error-code → translated message mapping in `src/lib/errors.ts`.

## Phase 4 — Crowdin (follow-up PR)

- Create Crowdin project, add `.github/workflows/crowdin.yml`.
- Add "How to translate" to `CONTRIBUTING.md`.
- First community language (Japanese suggested).

## Phase 5 — Polish (follow-up)

- Pseudo-locale for layout testing.
- CI check: fail if a non-en locale is missing keys present in en.

## Commit plan

1. `feat(i18n): phase 1 infra + config + selector` (Phase 1 all steps)
2. `feat(i18n): phase 2 core search UI in Chinese` (Phase 2)
3. `feat(i18n): phase 3 settings & setup modals` (Phase 3)
4. `feat(i18n): phase 4 crowdin integration` (Phase 4)

Each commit must keep `bun run build` green and the app runnable.