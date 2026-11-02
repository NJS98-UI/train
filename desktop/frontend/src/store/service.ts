// service 切片（§8.2）：running / busyLabel 只读 + 启停命令。
// serviceBusy 守卫：进行中禁用、finally 必复位（对齐 §6 第 3/27 条与 onDestroy 兜底语义）。
import { create } from 'zustand'
import { call } from '../ipc'
import { toast } from './toast'
import { SIZES } from '../theme/styles'

export interface ServiceStatus {
  running: boolean
  port?: number
  busy?: string | null
  shouldRun?: boolean
  hasUserIntent?: boolean
}

interface ServiceState {
  running: boolean
  port: number
  shouldRun: boolean
  hasUserIntent: boolean
  busy: string | null
  loaded: boolean
  error: string | null
  refresh: () => Promise<void>
  toggle: () => Promise<void>
  start: () => Promise<boolean>
  applyStatus: (s: Partial<ServiceStatus>) => void
}

/// Android 侧 500ms 后复位 busy + toast + redraw（§2.1 第 4 项）
const BUSY_RESET_DELAY = 500

export function serverUrl(port: number): string {
  return `http://127.0.0.1:${port}/v1`
}

export const useService = create<ServiceState>((set, get) => ({
  running: false,
  port: SIZES.port,
  shouldRun: false,
  hasUserIntent: false,
  busy: null,
  loaded: false,
  error: null,

  applyStatus(s) {
    set({
      running: s.running ?? get().running,
      port: s.port ?? get().port,
      shouldRun: s.shouldRun ?? get().shouldRun,
      hasUserIntent: s.hasUserIntent ?? get().hasUserIntent,
      loaded: true,
      error: null,
    })
  },

  async refresh() {
    try {
      const s = await call<ServiceStatus>('service_status')
      get().applyStatus(s)
    } catch (e) {
      set({ loaded: true, error: (e as Error).message })
    }
  },

  async toggle() {
    const wasRunning = get().running
    if (!get().loaded && !get().error) await get().refresh()
    if (get().busy) return // 守卫：进行中直接忽略
    set({ busy: wasRunning ? '停止中…' : '启动中…' })
    try {
      await call(wasRunning ? 'service_stop' : 'service_start')
      await new Promise((r) => setTimeout(r, BUSY_RESET_DELAY))
      await get().refresh()
      // 命令未实现/失败时也按最新状态给提示（页面不白屏）
      if (!get().error) toast(wasRunning ? '服务已停止' : '服务已启动')
    } catch (e) {
      toast((e as Error).message)
    } finally {
      set({ busy: null })
    }
  },

  async start() {
    if (get().busy) return false
    set({ busy: '启动中…' })
    try {
      await call('service_start')
      await new Promise((r) => setTimeout(r, BUSY_RESET_DELAY))
      await get().refresh()
      return get().running
    } catch (e) {
      toast((e as Error).message)
      return false
    } finally {
      set({ busy: null })
    }
  },
}))
