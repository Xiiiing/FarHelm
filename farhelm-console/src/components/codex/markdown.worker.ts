import { unified } from 'unified'
import remarkParse from 'remark-parse'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import remarkRehype from 'remark-rehype'
import rehypeKatex from 'rehype-katex'
import { normalizeMath } from './math'

const processor = unified().use(remarkParse).use(remarkGfm).use(remarkMath).use(remarkRehype).use(rehypeKatex, { trust: false, strict: 'ignore', maxExpand: 1000 })
self.onmessage = (event: MessageEvent<{ id: number; text: string }>) => {
  const { id, text } = event.data
  try {
    const source = normalizeMath(text)
    const tree = processor.runSync(processor.parse(source))
    // Bound layout work for a multi-megabyte plain paragraph as well as parsing
    // work. Offscreen chunks retain their measured height after first layout.
    tree.children = tree.children.flatMap((node) => {
      if (node.type !== 'element' || node.tagName !== 'p' || node.children.length !== 1 || node.children[0].type !== 'text' || node.children[0].value.length <= 8192) return [node]
      const value = Array.from(node.children[0].value)
      const chunks = []
      for (let offset = 0; offset < value.length; offset += 4096) chunks.push({ ...node, properties: { ...node.properties, className: ['long-paragraph-chunk'] }, children: [{ type: 'text' as const, value: value.slice(offset, offset + 4096).join('') }] })
      return chunks
    })
    self.postMessage({ id, tree })
  } catch { self.postMessage({ id, error: true }) }
}
