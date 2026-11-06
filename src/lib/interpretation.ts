import type { TFunction } from "i18next";
import type { InterpretationChip } from "../types";

/* The backend returns a ready-made English label for every interpretation
   chip. That label is fine for logs and tests, but the UI must follow the
   selected language, so rebuild it from the structured `kind` / `raw` fields.
   Unknown shapes fall back to the backend label rather than disappearing. */

const TYPE_KEYS = {
  "type:image": "interpretation.type.images",
  "ext:pdf": "interpretation.type.pdf",
  "type:media": "interpretation.type.media",
  "type:code": "interpretation.type.code",
  "type:doc": "interpretation.type.documents",
  "type:dir": "interpretation.type.folders",
  "type:archive": "interpretation.type.archives",
  "ext:log": "interpretation.type.logs",
} as const;

const PLACE_KEYS = {
  projects: "interpretation.place.projects",
  downloads: "interpretation.place.downloads",
  documents: "interpretation.place.documents",
  desktop: "interpretation.place.desktop",
  pictures: "interpretation.place.pictures",
  videos: "interpretation.place.videos",
  music: "interpretation.place.music",
  work: "interpretation.place.work",
} as const;

const WEEKDAY_KEYS = {
  monday: "interpretation.weekday.monday",
  tuesday: "interpretation.weekday.tuesday",
  wednesday: "interpretation.weekday.wednesday",
  thursday: "interpretation.weekday.thursday",
  friday: "interpretation.weekday.friday",
  saturday: "interpretation.weekday.saturday",
  sunday: "interpretation.weekday.sunday",
} as const;

export function interpretationLabel(
  chip: InterpretationChip,
  t: TFunction<"search">,
): string {
  const { kind, raw, label } = chip;

  if (kind === "sort") {
    if (raw === "sort:newest") return t("interpretation.sort.newest");
    if (raw === "sort:oldest") return t("interpretation.sort.oldest");
    if (raw === "sort:largest") return t("interpretation.sort.largest");
  }

  if (kind === "time") {
    if (label === "Modified: today") return t("interpretation.time.today");
    if (label === "Modified: yesterday") return t("interpretation.time.yesterday");
    if (label === "Modified: last 7 days") return t("interpretation.time.last7Days");
    if (label === "Modified: last 30 days") return t("interpretation.time.last30Days");
    if (label === "Modified: last year") return t("interpretation.time.lastYear");

    const weekday = /^Modified: last (\w+)$/i.exec(label);
    if (weekday) {
      const key = WEEKDAY_KEYS[weekday[1].toLowerCase() as keyof typeof WEEKDAY_KEYS];
      if (key) return t("interpretation.time.lastWeekday", { weekday: t(key) });
    }

    // The backend renders this chip as "Modified: days ago N", so accept that
    // shape (and the natural "Modified: N days ago" one) rather than leaving
    // an English label in a translated UI.
    const daysAgo =
      /^Modified: days ago (\d+)$/i.exec(label) ??
      /^Modified: (\d+) days ago$/i.exec(label);
    if (daysAgo) return t("interpretation.time.daysAgo", { count: Number(daysAgo[1]) });
  }

  if (kind === "size") {
    const size = /^size:([<>])([0-9.]+)(kb|mb|gb)$/i.exec(raw);
    if (size) {
      const value = `${size[2]} ${size[3].toUpperCase()}`;
      return size[1] === "<"
        ? t("interpretation.sizeUnder", { value })
        : t("interpretation.sizeOver", { value });
    }
  }

  if (kind === "visual") return t("interpretation.visual");

  if (kind === "type") {
    const key = TYPE_KEYS[raw as keyof typeof TYPE_KEYS];
    if (key) return t(key);
  }

  if (kind === "place") {
    const place = raw.replace(/^in:/, "").toLowerCase();
    const key = PLACE_KEYS[place as keyof typeof PLACE_KEYS];
    if (key) return t(key);
    return t("interpretation.in", { place });
  }

  if (kind === "alias") {
    return t("interpretation.alias", { term: chip.source || label.replace(/^Alias:\s*/i, "") });
  }

  return label;
}
