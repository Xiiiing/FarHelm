import hljs from 'highlight.js/lib/core'
import python from 'highlight.js/lib/languages/python'
import bash from 'highlight.js/lib/languages/bash'
import javascript from 'highlight.js/lib/languages/javascript'
import typescript from 'highlight.js/lib/languages/typescript'
import rust from 'highlight.js/lib/languages/rust'
import json from 'highlight.js/lib/languages/json'
import cpp from 'highlight.js/lib/languages/cpp'
for (const [name, language] of Object.entries({ python, bash, javascript, typescript, rust, json, cpp })) hljs.registerLanguage(name, language)
export function highlight(element: HTMLElement, language: string) {
  if (!hljs.getLanguage(language) || element.dataset.highlighted) return
  // highlight.js escapes source text; model HTML never enters this rendering path.
  hljs.highlightElement(element)
}
