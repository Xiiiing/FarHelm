import { CopyOutlined, CheckOutlined } from '@ant-design/icons'
import { Button } from 'antd'
import { memo, useEffect, useRef, useState, type ComponentProps, type ReactNode } from 'react'
import { Fragment, jsx, jsxs } from 'react/jsx-runtime'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import rehypeKatex from 'rehype-katex'
import { toJsxRuntime } from 'hast-util-to-jsx-runtime'
import { normalizeMath } from './math'
import { renderMarkdown } from './markdownService'
import 'katex/dist/katex.min.css'

function safeUrl(url?: string): string | undefined {
  if (!url) return undefined
  try { const parsed = new URL(url, location.origin); return ['http:', 'https:', 'mailto:'].includes(parsed.protocol) && !url.startsWith('//') ? url : undefined } catch { return undefined }
}

function CodeBlock({ children }: { children?: ReactNode }) {
  const ref = useRef<HTMLPreElement>(null)
  const [copied, setCopied] = useState(false)
  const [failed, setFailed] = useState(false)
  const [language, setLanguage] = useState('text')
  useEffect(() => {
    const code = ref.current?.querySelector('code')
    if (!code) return
    const match = /language-([\w+-]+)/.exec(code.className)
    const lang = match?.[1] ?? 'text'
    setLanguage(lang)
    let active = true
    if (match && (code.textContent?.length ?? 0) < 16000) void import('./highlight').then(({ highlight }) => { if (active) highlight(code, lang) })
    return () => { active = false }
  }, [children])
  useEffect(() => { if (!copied) return; const timer = setTimeout(() => setCopied(false), 2000); return () => clearTimeout(timer) }, [copied])
  return <div className="markdown-code"><div className="code-toolbar"><span>{language}</span><Button type="text" icon={copied ? <CheckOutlined /> : <CopyOutlined />} onClick={() => void navigator.clipboard.writeText(ref.current?.textContent ?? '').then(() => { setCopied(true); setFailed(false) }).catch(() => setFailed(true))}>{failed ? '复制失败，可选择文本' : copied ? '已复制' : '复制代码'}</Button></div><pre ref={ref} tabIndex={0}>{children}</pre></div>
}

const components: Components = {
  pre: CodeBlock,
  a: ({ href, children }) => { const safe = safeUrl(href); return safe ? <a href={safe} target="_blank" rel="noopener noreferrer">{children}</a> : <span>{children}</span> },
  img: ({ alt, src }) => <span className="markdown-image-label">{alt || '图片'}{safeUrl(src) && <> · <a href={safeUrl(src)} target="_blank" rel="noopener noreferrer">打开图片</a></>}</span>,
  table: ({ children }) => <div className="markdown-table" tabIndex={0} role="region" aria-label="表格"><table>{children}</table></div>,
  h1: ({ children }) => <h2>{children}</h2>,
  h2: ({ children }) => <h3>{children}</h3>,
  h3: ({ children }) => <h4>{children}</h4>,
}

function LargeMarkdown({ text }: { text: string }) {
  const [rendered, setRendered] = useState<{ text: string; node: ReactNode }>()
  useEffect(() => {
    let cancel: (() => void) | undefined
    const timer = setTimeout(() => {
      cancel = renderMarkdown(text, (tree) => setRendered({ text, node: tree ? toJsxRuntime(tree, { Fragment, jsx, jsxs, components, passNode: true }) as ReactNode : <div className="markdown-fallback">{text}</div> }))
    }, 100)
    return () => { clearTimeout(timer); cancel?.() }
  }, [text])
  return <div aria-busy={rendered?.text !== text}>{rendered?.node ?? <span className="message-role">正在排版长消息…</span>}</div>
}

const rehypePlugins: ComponentProps<typeof ReactMarkdown>['rehypePlugins'] = [[rehypeKatex, { trust: false, strict: 'ignore', maxExpand: 1000 }]]
export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return <div className="markdown-body">{text.length > 16000 ? <LargeMarkdown text={text} /> : <ReactMarkdown remarkPlugins={[remarkGfm, remarkMath]} rehypePlugins={rehypePlugins} components={components} skipHtml urlTransform={(url) => safeUrl(url) ?? ''}>{normalizeMath(text)}</ReactMarkdown>}</div>
})
