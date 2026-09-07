// Convert TeX delimiters outside fenced/inline code, preserving literal slashes.
export function normalizeMath(text: string): string {
  return text.replace(/(`{3,}|~{3,})[^\n]*\n[\s\S]*?(?:\n\1[^\n]*|$)|(`+)[\s\S]*?\2|\\\\|\\\[([\s\S]*?)\\\]|\\\(([^\n]*?)\\\)/g, (match, _fence, _inline, display: string | undefined, inline: string | undefined) => display !== undefined ? `\n$$\n${display}\n$$\n` : inline !== undefined ? `$${inline}$` : match)
}
