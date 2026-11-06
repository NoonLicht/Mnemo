import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

import enCommon from "../locales/en/common.json";
import enSearch from "../locales/en/search.json";
import enHome from "../locales/en/home.json";
import enResults from "../locales/en/results.json";
import enDetails from "../locales/en/details.json";
import enHelp from "../locales/en/help.json";
import enSettings from "../locales/en/settings.json";
import enSetup from "../locales/en/setup.json";
import enErrors from "../locales/en/errors.json";
import enMnemo from "../locales/en/mnemo.json";
import ruCommon from "../locales/ru/common.json";
import ruSearch from "../locales/ru/search.json";
import ruHome from "../locales/ru/home.json";
import ruResults from "../locales/ru/results.json";
import ruDetails from "../locales/ru/details.json";
import ruHelp from "../locales/ru/help.json";
import ruSettings from "../locales/ru/settings.json";
import ruSetup from "../locales/ru/setup.json";
import ruErrors from "../locales/ru/errors.json";
import ruMnemo from "../locales/ru/mnemo.json";


export const SUPPORTED_LANGUAGES = {
  ru: "Русский",
  en: "English",
} as const;

export type SupportedLanguage = keyof typeof SUPPORTED_LANGUAGES;

const resources = {
  en: {
    common: enCommon,
    search: enSearch,
    home: enHome,
    results: enResults,
    details: enDetails,
    help: enHelp,
    settings: enSettings,
    setup: enSetup,
    errors: enErrors,
    mnemo: enMnemo,
  },
  ru: {
    common: ruCommon,
    search: ruSearch,
    home: ruHome,
    results: ruResults,
    details: ruDetails,
    help: ruHelp,
    settings: ruSettings,
    setup: ruSetup,
    errors: ruErrors,
    mnemo: ruMnemo,
  },
};

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources,
    fallbackLng: "ru",
    supportedLngs: Object.keys(SUPPORTED_LANGUAGES),
    // Browsers report "zh-CN"/"zh-TW"; both should match the "zh" bundle.
    load: "languageOnly",
    defaultNS: "common",
    ns: [
      "common",
      "search",
      "home",
      "results",
      "details",
      "help",
      "settings",
      "setup",
      "errors",
      "mnemo",
    ],
    interpolation: { escapeValue: false },
    detection: {
      order: ["localStorage", "navigator"],
      // The hook owns persistence: writing here too would turn "Auto" into a
      // frozen detected language on the next launch.
      caches: [],
      lookupLocalStorage: "mnemo.language",
    },
    react: { useSuspense: false },
  });

export default i18n;
