import { Alert, Button, List, Modal, Typography } from 'antd'
import { useMutation, useQuery } from '@tanstack/react-query'
import { archiveSession, fetchArchivePreview, unarchiveSession, waitForCommand, type CodexSession } from '../../api/features'
import { keys, queryClient } from '../../api/cache'
import { errorText, sessionName } from './presentation'

export function ArchiveDialog({ csrf, session, onClose }: { csrf: string; session: CodexSession; onClose: () => void }) {
  const restoring = session.state === 'archived'
  const preview = useQuery({ queryKey: ['archive-preview', session.session_id], enabled: !restoring, queryFn: ({ signal }) => fetchArchivePreview(session.session_id, signal), staleTime: 0 })
  const action = useMutation({ mutationFn: async () => {
    await waitForCommand(await (restoring ? unarchiveSession(csrf, session.session_id) : archiveSession(csrf, session.session_id, preview.data!.fingerprint)))
    await Promise.all([queryClient.invalidateQueries({ queryKey: keys.session(session.session_id) }), queryClient.invalidateQueries({ queryKey: ['codex', 'sessions'] }), queryClient.invalidateQueries({ queryKey: keys.history(session.session_id) })])
  }, onSuccess: onClose })
  return <Modal title={restoring ? '恢复会话' : '归档会话'} open onCancel={onClose} okText={restoring ? '确认恢复' : '确认归档'} cancelText="取消" confirmLoading={action.isPending} okButtonProps={{ disabled: !restoring && (!preview.data?.can_archive || preview.isFetching || !!preview.error) }} onOk={() => action.mutate()}>
    <Typography.Paragraph strong>{sessionName(session)}</Typography.Paragraph>
    <Typography.Paragraph>{restoring ? '恢复这条会话，保留原会话 ID 和全部历史。关联子会话保持各自当前状态。' : '归档会同步到原生 Codex，并包含此会话派生的关联子会话。归档前会再次检查授权、保存状态、活动任务、排队输入和待触发调度。'}</Typography.Paragraph>
    {(preview.error || action.error) && <Alert type="warning" showIcon title={errorText(action.error ?? preview.error)} action={!restoring && <Button onClick={() => { action.reset(); void preview.refetch() }}>重新核对</Button>} />}
    {!restoring && <List loading={preview.isFetching} header={preview.data ? `影响范围：${preview.data.session_ids.length} 条会话` : '正在核对影响范围'} dataSource={preview.data?.session_ids ?? []} renderItem={(id) => <List.Item>{queryClient.getQueryData<CodexSession>(keys.session(id))?.title ?? id}</List.Item>} />}
  </Modal>
}
