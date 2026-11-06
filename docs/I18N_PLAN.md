# Mnemo i18n Plan

Chinese first, then N languages. Status: **shipped for English and Chinese (Phase 1).** Lazy loading, Intl formatting, community translation, and additional languages remain future work.

## Goals

- Ship a **fully working Chinese UI** on the main search screen without English leaking through.
- Make adding language #3 (Japanese, Spanish, …) a **mechanical change** — a locale folder plus one config entry — not another 500-key translation sprint.
- Let the community translate via a web UI (Crowdin) instead of PRs touching source code.
- Keep the bundle small: only the active language's strings should be loaded (deferred; Phase 1 bundles both eagerly).

## Non-goals (v1)

- Translating Rust backend error/doctor messages (they stay English; only UI chrome).
- Translating query syntax (`type:`, `in:`, `modified:`, `re:"..."`) — it is a search grammar, not prose.
- RTL layouts (Arabic/Hebrew). Revisit when a contributor asks.
- Bundling CJK fonts. Rely on OS fallback (Windows/macOS/Linux all ship CJK fonts).

## Architecture

- **Library:** `react-i18next` + `i18next` + `i18next-browser-languagedetector` (already installed).
- **Namespaces (per feature, flat keys, max 2–3 levels deep):**
  `common`, `search`, `home`, `results`, `details`, `help`, `settings`, `setup`, `errors`.
- **Loading:** Phase 1 imports both locale bundles eagerly; with two languages the extra weight is small. The planned follow-up is `i18next-resources-to-backend` dynamic imports so only the active language's namespace files load. Adding a language = adding a folder; adding a namespace = adding a file.
- **Type safety:** `i18next` TS augmentation in `src/i18n/types.ts` so `t("search.placeholder")` is checked at compile time and missing keys fail `tsc`.
- **Plurals/interpolation:** ICU-style via i18next (`_one`/`_other`, `{{count}}`). Chinese has no plural, but the same keys work for every language.
- **Formatting (future):** use `Intl.DateTimeFormat` / `NumberFormat` with the active locale (replace the remaining hardcoded `"en-US"` in `src/lib/format.ts` re-exports).
- **Fallback:** `fallbackLng: "en"` so a missing key never shows a raw key or breaks the UI.
- **Backend messages:** stay English. The UI maps known backend error codes to translated strings where it matters (e.g. "index not ready").

## File layout

```
src/
  i18n/
    config.ts            # i18next init, supportedLngs, detection, lazy backend
    types.ts             # TS augmentation (type-safe keys)
    index.ts             # re-exports
  locales/
    en/
      common.json
      search.json
      home.json
      results.json
      details.json
      help.json
      settings.json
      setup.json
      errors.json
    zh/
      ... (same files, Chinese values)
    ja/ ...              # future, empty until translated
```

Tauri's `public/` is not used; strings are bundled by Vite from `src/locales/`.

## Config & settings (mirror the `theme` pattern)

- Rust: add `language: Option<String>` to `MnemoConfig` in `src-tauri/src/config.rs`, default `None` (auto-detect), with `normalize_language()` and a v6→v7 migration (like `normalize_theme`).
- TS: add `language?: string` to `MnemoConfig` in `src/types.ts`.
- Hook: `src/hooks/useLanguage.ts` — reads the stored/config choice, applies it to i18next, and mirrors the picker to localStorage so the UI switches instantly; Settings persists it to `config.language` on Save.
- UI: a dropdown in `Settings → General` (next to theme). Options: "Auto (system)", "English", "中文", … driven by `supportedLngs`.
- `useLanguage` sets `document.documentElement.lang` whenever the language changes.

## Crowdin workflow (community translations)

- Create a free Crowdin project (OSS tier) pointed at `src/locales/en/*.json` as source.
- Add `.github/workflows/crowdin.yml` using `crowdin/github-action@v2`:
  - On push to `master` touching `src/locales/en/**`: upload sources.
  - Weekly (or on PR merge): download translations, open a PR with new/updated locale files.
- Translators work in Crowdin's web UI; no Git knowledge needed. New languages appear as new folders automatically.
- `CONTRIBUTING.md` gets a one-paragraph "How to translate" pointing to Crowdin.

## Phases

### Phase 1 — Infra (shipped)
- Install/config i18next, create `src/i18n/*`, empty `en/*.json` + `zh/*.json`.
- Add `language` to Rust config + TS types + normalizer + migration.
- `useLanguage` hook + settings dropdown.
- `index.html` `lang` binding.
- **Deliverable:** language switcher works; everything still English (fallback). CI green.

### Phase 2 — Core search UI in Chinese (shipped)
Translate these namespaces fully to zh:
- `common` (buttons, toasts, units)
- `search` (search bar, slash menu, context menu, filters, chips)
- `home` (home view, empty state, recent)
- `results` (result controls, item labels, sort)
- `details` (details dialog)
- `help` (help dialog)
Wire `t()` into: `App.tsx`, `HomeView`, `SearchBar`, `EmptyState`, `ResultControls`, `ResultItem`, `FilterChips`, `CategoryChips`, `KeyHintStrip`, `HelpDialog`, `DetailsDialog`, `ContextMenu`.

### Phase 3 — Heavy modals (shipped)
- `settings` + `setup` namespaces → translate `SettingsModal.tsx` (~62 KB) and `SetupModal.tsx` (~35 KB). These are the bulk of the strings; do them as a dedicated PR.

### Phase 4 — Backend + extras (partly shipped: surfaced error strings; Crowdin pending)
- Map backend error codes to translated strings in the UI.
- Crowdin onboarding + first community language (Japanese likely first given CJK overlap).
- `Intl` formatting audit.

### Phase 5 — Polish
- Pseudo-locale (`xx`) for layout testing (long German strings break UI).
- RTL prep (logical CSS properties) if Arabic/Hebrew requested.

## Effort summary

| Phase | Effort | Output |
|---|---|---|
| 1 Infra | 1–2 days | Switcher works, all English |
| 2 Core UI | 2–4 days | Chinese on main screen |
| 3 Modals | 3–5 days | Fully Chinese app |
| 4 Backend+Crowdin | 2–3 days | Community-scalable |
| 5 Polish | 1–2 days | Robust |

**Total to "fully Chinese": ~2 weeks.** Phase 1+2 (~1 week) is the smallest shippable slice.

## Risks & mitigations

- **Half-translated UI looks broken.** Mitigation: namespaces are all-or-nothing per release; untranslated namespaces fall back to English, which is acceptable and clearly better than mixed sentences.
- **Key drift.** Mitigation: type-safe keys + a CI check (`i18next-parser` or a small script) that fails if a non-English locale is missing keys present in `en`.
- **Layout breakage from long words.** Mitigation: pseudo-locale test in Phase 5; use flexible layouts (no fixed widths on text).
- **Backend strings.** Mitigation: keep English; only translate the handful the UI surfaces via error-code mapping.
- **Scope creep into N languages.** Mitigation: ship zh first; other languages arrive via Crowdin without code changes.

## First deliverable

**Phase 1 + 2** as one PR: working language switcher + Chinese on the search screen, English fallback elsewhere. Reviewable, testable, and it unblocks community translation of the rest.

## Open questions for the maintainer

1. Should the default be "Auto (system language)" or always English? (Recommend: Auto, fallback English.)
2. Should we ship Crowdin integration in the same PR or a follow-up? (Recommend: follow-up, after Phase 2 lands.)
3. Do we want a `zh-TW` (Traditional) variant alongside `zh` (Simplified)? (Crowdin handles this trivially once infra exists.)