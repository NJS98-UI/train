//! 事件订阅（design.md §IPC 契约 / §8.2）：service-state / credits-snapshot / checkin-result /
//! models-updated / notice。notice 直接弹 toast；其余写入对应切片，由组件局部订阅原地更新。

import { onEvent, type Unlisten } from '../ipc'
import { useService } from './service'
import { useCredits } from './credits'
import { useModels } from './models'
import { useTrae } from './trae'
import { useQoder } from './qoder'
import { toast } from './toast'

export function subscribeEvents(): () => void {
  const unlisteners: Unlisten[] = []

  unlisteners.push(
    onEvent('service-state', (payload: { running?: boolean; busy?: string | null }) => {
      useService.getState().applyStatus(payload ?? {})
    }),
  )

  unlisteners.push(
    onEvent('credits-snapshot', (payload) => {
      useCredits.getState().applySnapshot(payload ?? {})
    }),
  )

  unlisteners.push(
    onEvent('checkin-result', (payload: { platform?: string; summary?: string; text?: string }) => {
      const text = payload?.text ?? payload?.summary
      // 对齐 §3「后台补签」文案：已自动补签：{summary}
      toast(text ? `已自动补签：${text}` : '签到结果已更新')
      void useCredits.getState().load()
      void reloadAccountStates()
    }),
  )

  unlisteners.push(
    onEvent('models-updated', () => {
      void useModels.getState().load()
    }),
  )

  unlisteners.push(
    onEvent('notice', (payload: unknown) => {
      const text = typeof payload === 'string' ? payload : ((payload as { text?: string })?.text ?? '')
      if (text) toast(text)
    }),
  )

  // 账号态由后端在签到/登录后广播，桌面端复用 models-updated 之外的 notice 通道即可；
  // 这里额外提供一个手动刷新入口，供页面在需要时调用。
  return () => {
    for (const u of unlisteners) {
      try {
        u()
      } catch {
        /* ignore */
      }
    }
  }
}

export async function reloadAccountStates(): Promise<void> {
  await Promise.all([useTrae.getState().load(), useQoder.getState().load()])
}
