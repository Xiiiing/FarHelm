import { useRef, type ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Button, Image, Select, Space, Tag, Upload } from 'antd'
import { fetchNativeControl, operateNative, waitForCommand } from '../../api/features'
import type { SessionDraft } from './useOperations'
import { NativeSettings } from './NativeSettings'
import { errorText } from './presentation'

export type DraftImage = { id: string; name: string; url: string; state: 'uploading' | 'ready' | 'failed' }
type Props = { csrf: string; session: string; draft: SessionDraft; onDraft: (change: (old: SessionDraft) => SessionDraft) => void; disabled: boolean; enabled: boolean; children: ReactNode }
export function NativeComposer({ csrf, session, draft, onDraft, disabled, enabled, children }: Props) {
  const reservations = useRef(new Set<string>())
  const skills = useQuery({ queryKey: ['codex', 'native', session, 'skills'], enabled, queryFn: ({ signal }) => fetchNativeControl<{ data: { skills?: { skillId: string; name: string }[] }[] }>(session, 'skills', signal) })
  const upload = async (file: File) => {
    if (disabled) return
    if (!['image/png', 'image/jpeg', 'image/webp'].includes(file.type) || file.size < 1 || file.size > 20 * 1024 * 1024) { onDraft(old => ({ ...old, error: '仅支持 PNG、JPEG、WebP，每张最大 20 MiB。' })); return }
    if (new Set([...(draft.images ?? []).map(image => image.id), ...reservations.current]).size >= 4) { onDraft(old => ({ ...old, error: '每次最多四张图片。' })); return }
    const id = `att_${crypto.randomUUID().replaceAll('-', '')}`
    reservations.current.add(id)
    const url = URL.createObjectURL(file)
    onDraft(old => ({ ...old, images: [...(old.images ?? []), { id, name: file.name, url, state: 'uploading' }] }))
    const run = async (operation: Parameters<typeof operateNative>[2]) => waitForCommand(await operateNative(csrf, session, operation))
    try {
      await run({ operation: 'attachment_begin', attachment_id: id, mime_type: file.type, size_bytes: file.size, ephemeral: false })
      for (let offset = 0; offset < file.size; offset += 256 * 1024) {
        const bytes = new Uint8Array(await file.slice(offset, offset + 256 * 1024).arrayBuffer())
        let binary = ''; for (let start = 0; start < bytes.length; start += 0x8000) binary += String.fromCharCode(...bytes.subarray(start, start + 0x8000))
        await run({ operation: 'attachment_chunk', attachment_id: id, offset, data_base64: btoa(binary) })
      }
      await run({ operation: 'attachment_finish', attachment_id: id })
      onDraft(old => ({ ...old, images: old.images?.map(image => image.id === id ? { ...image, state: 'ready' } : image) }))
    } catch (error) { onDraft(old => ({ ...old, error: errorText(error), images: old.images?.map(image => image.id === id ? { ...image, state: 'failed' } : image) })) }
    finally { reservations.current.delete(id) }
  }
  return <div onPaste={event => {
    if (!enabled || disabled) return
    const images = Array.from(event.clipboardData.files).filter(file => file.type.startsWith('image/'))
    if (images.length) { event.preventDefault(); for (const file of images) void upload(file) }
  }}>
    {enabled && <Space wrap className="native-composer-tools">
      <Select aria-label="本次指令 Skills" mode="multiple" allowClear showSearch optionFilterProp="label" placeholder="Skills" disabled={disabled} value={draft.skills ?? []} onChange={skills => onDraft(old => ({ ...old, skills: skills.slice(0, Math.max(0, 8 - (old.images?.length ?? 0))) }))} options={skills.data?.data.data.flatMap(group => group.skills ?? []).map(skill => ({ value: skill.skillId, label: skill.name }))} />
      <NativeSettings csrf={csrf} session={session} disabled={disabled} />
      <Upload accept="image/png,image/jpeg,image/webp" multiple showUploadList={false} beforeUpload={file => { void upload(file); return false }}><Button disabled={disabled || (draft.images?.length ?? 0) >= 4}>添加图片</Button></Upload>
      <Button disabled={disabled} onClick={() => { void navigator.clipboard.read().then(async items => { for (const item of items) for (const type of item.types.filter(type => ['image/png', 'image/jpeg', 'image/webp'].includes(type))) { const blob = await item.getType(type); void upload(new File([blob], '粘贴图片', { type })) } }).catch(() => onDraft(old => ({ ...old, error: '剪贴板读取失败，可使用添加图片。' }))) }}>粘贴图片</Button>
      {draft.images?.map(image => <Space key={image.id}><Image width={44} height={44} src={image.url} alt={image.name} /><Tag>{image.state === 'ready' ? '已上传' : image.state === 'failed' ? '上传失败' : '上传中'}</Tag><Button disabled={draft.sending} onClick={() => { URL.revokeObjectURL(image.url); onDraft(old => ({ ...old, images: old.images?.filter(item => item.id !== image.id) })) }}>移除</Button></Space>)}
    </Space>}
    {children}
  </div>
}
