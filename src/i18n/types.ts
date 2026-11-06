import "i18next";
import common from "../locales/en/common.json";
import search from "../locales/en/search.json";
import home from "../locales/en/home.json";
import results from "../locales/en/results.json";
import details from "../locales/en/details.json";
import help from "../locales/en/help.json";
import settings from "../locales/en/settings.json";
import setup from "../locales/en/setup.json";
import errors from "../locales/en/errors.json";
import mnemo from "../locales/en/mnemo.json";

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
      mnemo: typeof mnemo;
    };
  }
}
