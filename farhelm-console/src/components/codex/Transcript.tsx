import { CheckCircleOutlined, CloseCircleOutlined, CodeOutlined, CopyOutlined, LoadingOutlined, ToolOutlined } from '@ant-design/icons'
import { Actions, ThoughtChain } from '@ant-design/x'
import { useVirtualizer } from '@tanstack/react-virtual'
import { Button, Tag, Tooltip } from 'antd'
import { memo, useLayoutEffect, useRef, useState, type RefObject } from 'react'
import type { TranscriptItem, TranscriptPage, TranscriptTurn } from '../../api/features'
import { Markdown } from './Markdown'
import { stateNames } from './presentation'

const isTool = (item: TranscriptItem) => item.kind === 'command_summary' || item.kind === 'file_change_summary'
function Tools({ items, onAnchor }: { items: TranscriptItem[]; onAnchor: () => void }) {
  const [expanded, setExpanded] = useState<string[]>([])
  const change = (keys: string[]) => { onAnchor(); setExpanded(keys) }
  const failed = items.some((item) => item.status === 'failed' || item.status === 'declined' || (item.exit_code !== undefined && item.exit_code !== 0) || /exit [1-9]/.test(item.text))
  const running = items.some((item) => item.status === 'inProgress')
  const duration = items.reduce((sum, item) => sum + (item.duration_ms ?? 0), 0)
  return <ThoughtChain className={`execution-summary ${expanded.length ? 'is-open' : ''} ${failed ? 'has-failure' : ''}`} line={false} expandedKeys={expanded} onExpand={change} items={[{ key: 'tools', collapsible: true, status: failed ? 'error' : running ? 'loading' : 'success', icon: <ToolOutlined />, title: <Button type="text" className="tool-group-toggle" aria-expanded={expanded.length > 0} onClick={(event) => { event.stopPropagation(); change(expanded.length ? [] : ['tools']) }}><span className="tool-group-label"><span>执行过程 · {items.length} 项</span><span className={failed ? 'status-error' : 'tool-result'}>{failed ? <><CloseCircleOutlined /> 含失败</> : running ? <><LoadingOutlined /> 进行中</> : <><CheckCircleOutlined /> 已结束</>}</span>{duration > 0 && <span className="tool-duration">{(duration / 1000).toLocaleString()} 秒</span>}</span></Button>, content: <ol className="tool-items">{items.map((item) => <li key={item.item_id}><div className="tool-item-head"><span>{item.kind === 'command_summary' ? '命令' : '文件变更'}</span>{item.status && <Tag color={item.status === 'failed' ? 'error' : undefined}>{stateNames[item.status] ?? '已记录'}</Tag>}</div><pre tabIndex={0}>{item.text}</pre></li>)}</ol> }]} />
}

const Message = memo(function Message({ item }: { item: TranscriptItem }) {
  const [copied, setCopied] = useState(false)
  const role = item.kind === 'user_message' ? 'user' : item.kind === 'error' ? 'error' : 'assistant'
  return <article className={`codex-message ${role}`}><div className="message-role">{role === 'assistant' && <span className="assistant-mark"><CodeOutlined /></span>}{role === 'user' ? '你' : role === 'error' ? '执行错误' : 'Codex'}{item.streaming && <span className="stream-caret" aria-label="正在回复" />}</div><div className="message-body">{role === 'user' ? <div className="user-text">{item.text}</div> : <Markdown text={item.text} />}</div>{role === 'assistant' && !item.streaming && <Actions className="message-actions" fadeIn={false} items={[{ key: 'copy', actionRender: <Tooltip title={copied ? '已复制' : '复制回复'}><Button type="text" aria-label={copied ? '已复制回复' : '复制回复'} icon={copied ? <CheckCircleOutlined /> : <CopyOutlined />} onClick={() => { void navigator.clipboard.writeText(item.text).then(() => setCopied(true), () => setCopied(false)) }}><span>{copied ? '已复制' : '复制'}</span></Button></Tooltip> }]} />}</article>
})

export type TranscriptPosition = { reveal: (turn: string) => boolean }
type Props = { turns: TranscriptTurn[]; scroll: RefObject<HTMLDivElement | null>; position: RefObject<TranscriptPosition | undefined>; continuation?: TranscriptPage['continuation']; onContinue: () => void; loading: boolean; onAnchor: () => void }
const Turn = memo(function Turn({ turn, latest, continuation, loading, onContinue, onAnchor }: Omit<Props, 'turns' | 'scroll' | 'position'> & { turn: TranscriptTurn; latest: boolean }) {
  const tools = turn.items.filter(isTool)
  const firstTool = tools[0]?.item_id
  return <section className="codex-turn" data-turn-id={turn.turn_id} aria-label={`对话 ${turn.turn_id.slice(0, 8)}`}><header className={`turn-heading ${!turn.started_at_unix && turn.status === 'completed' ? 'heading-quiet' : ''}`}><h2 className="sr-only">{latest ? '最新对话' : '历史对话'}</h2>{turn.started_at_unix && <time dateTime={new Date(turn.started_at_unix * 1000).toISOString()}>{new Date(turn.started_at_unix * 1000).toLocaleString('zh-CN', { month: 'long', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false })}</time>}{turn.status !== 'completed' && <span className={`turn-state ${['failed', 'orphaned'].includes(turn.status) ? 'status-error' : ''}`}>{stateNames[turn.status] ?? '状态未知'}</span>}</header>{turn.items.map((item) => isTool(item) && item.item_id !== firstTool ? null : <div className="transcript-part" key={item.item_id} data-message-key={`${turn.turn_id}:${item.item_id}`}>
    {isTool(item) ? <Tools items={tools} onAnchor={onAnchor} /> : <Message item={item} />}
    {continuation?.kind === 'message' && continuation.turn_id === turn.turn_id && item.item_id === continuation.item_id && <Button className="continue-message" loading={loading} onClick={onContinue}>继续加载此消息</Button>}
  </div>)}</section>
})

export function Transcript({ turns, scroll, position, ...props }: Props) {
  const container = useRef<HTMLDivElement>(null)
  const [margin, setMargin] = useState(0)
  const enabled = turns.length > 40
  // Native library measurements are keyed by turn, so appending text never remounts other turns.
  // eslint-disable-next-line react-hooks/incompatible-library -- Virtualizer owns mutable measurements and must stay outside compiler memoization.
  const virtual = useVirtualizer({ count: turns.length, getScrollElement: () => scroll.current, getItemKey: (index) => turns[index].turn_id, estimateSize: () => 480, overscan: 4, enabled, scrollMargin: margin })
  useLayoutEffect(() => {
    position.current = { reveal: (turn) => {
      const index = turns.findIndex((row) => row.turn_id === turn)
      if (!enabled || index < 0) return false
      virtual.scrollToIndex(index, { align: 'start' }); return true
    } }
    return () => { position.current = undefined }
  }, [turns, position, enabled, virtual])
  useLayoutEffect(() => { if (container.current && scroll.current) setMargin(container.current.getBoundingClientRect().top - scroll.current.getBoundingClientRect().top + scroll.current.scrollTop) }, [turns.length, scroll])
  return <div className={`codex-transcript ${enabled ? 'virtual-transcript' : ''}`} ref={container} style={enabled ? { height: virtual.getTotalSize() } : undefined}>
    {enabled ? virtual.getVirtualItems().map((row) => <div key={row.key} ref={virtual.measureElement} data-index={row.index} className="virtual-turn" style={{ transform: `translateY(${row.start - margin}px)` }}><Turn {...props} turn={turns[row.index]} latest={row.index === turns.length - 1} /></div>) : turns.map((turn, index) => <Turn key={turn.turn_id} {...props} turn={turn} latest={index === turns.length - 1} />)}
  </div>
}
