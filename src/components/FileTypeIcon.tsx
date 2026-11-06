import React from "react";
import {
  AudioLines, File, FileArchive, FileCode, FileText, Film, Folder, Image,
  Presentation, Table, type LucideIcon,
} from "lucide-react";
import { classify } from "../lib/categories";

/* File-type mark. One muted ink for every category, so the type reads from
   the glyph alone. `bare` drops the frame, used on cards where the tile
   already provides the background. */

function iconFor(filename: string, isDir: boolean): LucideIcon {
  const cat = classify({ is_dir: isDir, filename });
  const ext = filename.includes(".") ? (filename.split(".").pop() ?? "").toLowerCase() : "";
  switch (cat) {
    case "folder":
      return Folder;
    case "image":
      return Image;
    case "code":
      return FileCode;
    case "archive":
      return FileArchive;
    case "media":
      return ["mp4", "mov", "avi", "mkv", "webm", "m4v"].includes(ext) ? Film : AudioLines;
    case "doc":
      if (["xls", "xlsx", "csv", "tsv", "ods", "numbers"].includes(ext)) return Table;
      if (["ppt", "pptx", "odp", "key"].includes(ext)) return Presentation;
      return FileText;
    default:
      return File;
  }
}

export const FileTypeIcon: React.FC<{
  result: { is_dir: boolean; filename: string };
  size?: number;
  bare?: boolean;
}> = ({ result, size = 32, bare = false }) => {
  const Icon = iconFor(result.filename, result.is_dir);
  const glyph = Math.round(size * 0.46);
  return (
    <span
      aria-hidden="true"
      style={{ width: size, height: size }}
      className={`flex shrink-0 items-center justify-center text-muted ${
        bare ? "" : "rounded-control border border-line bg-elevated"
      }`}
    >
      <Icon width={glyph} height={glyph} strokeWidth={1.5} />
    </span>
  );
};
