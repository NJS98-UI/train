// credits 切片（§8.2）：AccountCredits 快照（loading / at）+ quotaLoading + checkinBusy + qoderClaimBusy。
// 快照来源：credits_snapshot / credits_refresh_all + 事件 credits-snapshot。
import { create } from 'zustand'
import { call } from '../ipc'
import { toast } from './toast'

export interface CreditsSnapshot {
  loading?: boolean
  at?: number
  trae?: number | null
  qoder?: number | null
  total?: number | null
  error?: string | null
}

interface CreditsSlice {
  snapshot: CreditsSnapshot | null
  quotaLoading: boolean
  checkinBusy: boolean
  qoderClaimBusy: boolean
  error: string | null
  load: () => Promise<void>
  refreshAll: () => Promise<void>
  applySnapshot: (s: CreditsSnapshot) => void
  claimAllTrae: () => Promise<void>
  claimAllQoder: () => Promise<void>
  claimOneQoder: (uid: string) => Promise<void>
  diagnoseTrae: () => Promise<string | null>
  diagnoseQoder: () => Promise<string | null>
}

export const useCredits = create<CreditsSlice>((set, get) => ({
  snapshot: null,
  quotaLoading: false,
  checkinBusy: false,
  qoderClaimBusy: false,
  error: null,

  applySnapshot(s) {
    set({ snapshot: s, quotaLoading: s.loading ?? get().quotaLoading, error: null })
  },

  async load() {
    try {
      const snapshot = await call<CreditsSnapshot>('credits_snapshot')
      get().applySnapshot(snapshot)
    } catch (e) {
      set({ error: (e as Error).message })
    }
  },

  /// 点「刷新」：toast `正在查询全部账号积分…`，结果原地更新（不 remount）
  async refreshAll() {
    if (get().quotaLoading) return
    set({ quotaLoading: true })
    toast('正在查询全部账号积分…')
    try {
      const snapshot = await call<CreditsSnapshot>('credits_refresh_all', { force: true })
      get().applySnapshot({ ...(get().snapshot ?? {}), ...snapshot, loading: false })
    } catch (e) {
      set({ error: (e as Error).message, quotaLoading: false })
    } finally {
      set({ quotaLoading: false })
    }
  },

  /// 一键签到（§2.2 第 7 项 claimAllCheckin 的桌面合并形态：后端 batch 覆盖两平台账号）
  async claimAllTrae() {
    if (get().checkinBusy) return
    set({ checkinBusy: true })
    toast('正在签到全部账号…')
    try {
      const r = await call<{ okCount: number; allOk: boolean; results?: unknown[] }>('checkin_claim_all')
      toast(r.allOk ? `全部签到完成（${r.okCount} 个成功）` : `签到部分完成（${r.okCount} 个成功）`)
    } catch (e) {
      toast((e as Error).message)
    } finally {
      set({ checkinBusy: false })
      await get().load()
    }
  },

  async claimAllQoder() {
    if (get().qoderClaimBusy) return
    set({ qoderClaimBusy: true })
    try {
      const r = await call<{ total: number; okCount: number; date?: string }>('qoder_claim_all')
      toast(r.okCount > 0 ? `${r.okCount}/${r.total} 个账号领取成功` : '无可领额度（已领或不符）')
    } catch (e) {
      toast((e as Error).message)
    } finally {
      set({ qoderClaimBusy: false })
      await get().load()
    }
  },

  async claimOneQoder(uid: string) {
    if (get().qoderClaimBusy) return
    set({ qoderClaimBusy: true })
    try {
      const r = await call<{ status: string; amount?: number; message?: string }>('qoder_claim_one', { uid })
      if (r.status === 'ok') toast(`领取成功 +${r.amount ?? 0} Credits`)
      else if (r.status === 'claimed') toast(r.message ?? '今日已领取')
      else toast(r.message ?? '领取失败')
    } catch (e) {
      toast(`领取失败：${(e as Error).message}`)
    } finally {
      set({ qoderClaimBusy: false })
      await get().load()
    }
  },

  async diagnoseTrae() {
    try {
      return await call<string>('trae_diagnose')
    } catch (e) {
      toast((e as Error).message)
      return null
    }
  },

  async diagnoseQoder() {
    try {
      return await call<string>('qoder_diagnose')
    } catch (e) {
      toast((e as Error).message)
      return null
    }
  },
}))

/// 「聊天转发后端」三态（§2.2 第 4 项）
export const useBackend = create<{ backend: string; set: (b: string) => Promise<void> }>((set) => ({
  backend: 'auto',
  async set(b) {
    try {
      const cur = await call<string>('backend_set', { backend: b })
      set({ backend: cur })
      toast(`转发后端：${b === 'trae' ? 'Trae' : b === 'qoder' ? 'Qoder' : '自动'}`)
    } catch (e) {
      toast((e as Error).message)
    }
  },
}))

/// 自动换号（§2.2 第 5 项；只读+set，值≥5）
export const useAutoSwitch = create<{ enabled: boolean; threshold: number; load: () => Promise<void>; toggle: () => Promise<void>; setThreshold: (n: number) => Promise<void> }>((set, get) => ({
  enabled: false,
  threshold: 5,
  async load() {
    try {
      const r = await call<{ enabled: boolean; threshold: number }>('auto_switch_get')
      set({ enabled: r.enabled, threshold: r.threshold })
    } catch {
      /* 后端未就绪时保持默认 */
    }
  },
  async toggle() {
    try {
      const r = await call<{ enabled: boolean; threshold: number }>('auto_switch_set', { enabled: !get().enabled })
      set({ enabled: r.enabled })
      toast(r.enabled ? '已开启自动换号' : '已关闭自动换号')
    } catch (e) {
      toast((e as Error).message)
    }
  },
  async setThreshold(n) {
    try {
      const r = await call<{ enabled: boolean; threshold: number }>('auto_switch_set', { threshold: n })
      set({ threshold: r.threshold })
    } catch (e) {
      toast((e as Error).message)
    }
  },
}))

/// 定时领取（§2.7 第 7 项：两平台各自启用 + HH:mm；后端字段以 checkin_schedule_* 为准）
export async function scheduleGet(platform: string): Promise<{ enabled: boolean; hhmm: string } | null> {
  try {
    return await call<{ enabled: boolean; hhmm: string }>('checkin_schedule_get', { platform })
  } catch {
    return null
  }
}

export async function scheduleSet(platform: string, patch: { enabled?: boolean; hhmm?: string }): Promise<void> {
  try {
    const r = await call<{ enabled: boolean; hhmm: string }>('checkin_schedule_set', { platform, ...patch })
    if (patch.enabled === true) toast(`已开启，每天 ${r.hhmm}`)
    if (patch.enabled === false) toast('已关闭定时领取')
    else if (patch.hhmm) toast(`定时领取：每天 ${r.hhmm}`)
  } catch (e) {
    toast((e as Error).message)
  }
}

/// 导出/导入（§2.2 第 6 项：桌面走 IPC 文件对话框等价：导出落盘返回路径，导入读文件文本传入）
export async function backupExport(): Promise<void> {
  toast('账号 JSON 已导出，已复制到剪贴板。')
  try {
    const r = await call<{ path: string }>('backup_export')
    const text = await fetch(`/api/backup/raw?path=${encodeURIComponent(r.path)}`).then((x) => x.text()).catch(() => '')
    if (text) await navigator.clipboard.writeText(text).catch(() => undefined)
  } catch (e) {
    toast(`导出失败：${(e as Error).message}`)
  }
}

export async function backupImport(data: string): Promise<void> {
  toast('正在导入…')
  try {
    const r = await call<{ traeN: number; qoderN: number }>('backup_import', { data })
    toast(`已导入：Trae ${r.traeN} 个 · Qoder ${r.qoderN} 个`)
  } catch (e) {
    toast(`导入失败：${(e as Error).message}`)
  }
}
