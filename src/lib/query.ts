/* Build an `in:"<path>"` filter the backend lexer reads back verbatim.
   Inside quotes the lexer treats `\` as an escape before `"` or `\`, so both
   get escaped here. Without this, a picked Windows folder like `C:\Users\me`
   could be misread. */
export function inFilter(path: string): string {
  return `in:"${path.replace(/[\\"]/g, "\\$&")}"`;
}
