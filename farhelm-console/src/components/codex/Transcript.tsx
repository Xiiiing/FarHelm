import { CheckCircleOutlined, CloseCircleOutlined, LoadingOutlined, ToolOutlined } from '@ant-design/icons'
import { Button, Collapse, Tag } from 'antd'
import { memo } from 'react'
import type { TranscriptItem, TranscriptPage, TranscriptTurn } from '../../api/features'
import { Markdown } from './Markdown'
import { stateNames } from './presentation'

function groupItems(items: TranscriptItem[]) {
  const groups: { tools: boolean; items: TranscriptItem[] }[] = []
  for (const item of items) {
    const tools = item.kind === 'command_summary' || item.kind === 'file_change_summary'
    const previous = groups.at(-1)
    if (tools && previous?.tools) previous.items.push(item)
    else groups.push({ tools, items: [item] })
  }
  return groups
}

function Tools({ items, onAnchor }: { items: TranscriptItem[]; onAnchor: () => void }) {
  const failed = items.some((item) => item.status === 'failed' || item.status === 'declined' || (item.exit_code !== undefined && item.exit_code !== 0) || /exit [1-9]/.test(item.text))
  const running = items.some((item) => item.status === 'inProgress')
  const duration = items.reduce((sum, item) => sum + (item.duration_ms ?? 0), 0)
  return <Collapse className="execution-summary" ghost onChange={onAnchor} items={[{ key: 'tools', label: <div className="tool-group-label"><ToolOutlined /><span>执行过程 · {items.length} 项</span><span className={failed ? 'status-error' : 'tool-result'}>{failed ? <><CloseCircleOutlined /> 含失败</> : running ? <><LoadingOutlined /> 进行中</> : <><CheckCircleOutlined /> 已结束</>}</span>{duration > 0 && <span className="tool-duration">{(duration / 1000).toLocaleString()} 秒</span>}</div>, children: <ol className="tool-items">{items.map((item) => <li key={item.item_id}><div className="tool-item-head"><span>{item.kind === 'command_summary' ? '命令' : '文件变更'}</span>{item.status && <Tag color={item.status === 'failed' ? 'error' : undefined}>{stateNames[item.status] ?? '已记录'}</Tag>}</div><pre tabIndex={0}>{item.text}</pre></li>)}</ol> }]} />
}

const Message = memo(function Message({ item }: { item: TranscriptItem }) {
  const role = item.kind === 'user_message' ? 'user' : item.kind === 'error' ? 'error' : 'assistant'
  return <article className={`codex-message ${role}`}><div className="message-role">{role === 'user' ? '你' : role === 'error' ? '执行错误' : 'Codex'}{item.streaming && <span className="stream-caret" aria-label="正在回复" />}</div><div className="message-body">{role === 'user' ? <div className="user-text">{item.text}</div> : <Markdown text={item.text} />}</div></article>
})

export function Transcript({ turns, continuation, onContinue, loading, onAnchor }: { turns: TranscriptTurn[]; continuation?: TranscriptPage['continuation']; onContinue: () => void; loading: boolean; onAnchor: () => void }) {
  return <div className="codex-transcript">{turns.map((turn, index) => <section className="codex-turn" key={turn.turn_id} data-turn-id={turn.turn_id} aria-label={`对话 ${turn.turn_id.slice(0, 8)}`}><header className="turn-heading"><h2>{index === turns.length - 1 ? '最新对话' : '历史对话'}</h2>{turn.started_at_unix && <time>{new Date(turn.started_at_unix * 1000).toLocaleString()}</time>}<span className="turn-state">{stateNames[turn.status] ?? '状态未知'}</span></header>{groupItems(turn.items).map((group) => <div className="transcript-part" key={group.items[0].item_id} data-message-key={`${turn.turn_id}:${group.items[0].item_id}`}>
    {group.tools ? <Tools items={group.items} onAnchor={onAnchor} /> : <Message item={group.items[0]} />}
    {continuation?.kind === 'message' && continuation.turn_id === turn.turn_id && group.items.some((i) => i.item_id === continuation.item_id) && <Button className="continue-message" loading={loading} onClick={onContinue}>继续加载此消息</Button>}
  </div>)}</section>)}</div>
}
