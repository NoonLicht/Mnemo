/* Backend hints arrive as strings, sometimes decorated with an emoji
   (e.g. `📁 Documents`). We never render the emoji — parse out the label. */

export function hintToken(hint: string): { label: string; isDir: boolean } {
  const isDir = hint.includes("📁") || hint.toLowerCase().startsWith("in ");
  const label = hint.replace(/[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]/gu, "").trim();
  return { label: label || hint, isDir };
}
