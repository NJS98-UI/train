// trae 切片（§8.2）：账号列表 / activeUid / authFailedUid 按 uid 存（不按页面存）。
// 命令在后端未实现时 call 抛错 → 捕获进 error 字段，页面据此展示合理占位（不白屏）。
import { create } from 'zustand'
import { call } from '../ipc'
import { toast } from './toast'
import { openExternal } from '../ipc'

export interface TraeAccountView {
  uid?: string
  nickname?: string
  regionId?: string
  hasAccessToken?: boolean
  accessToken?: string
  expiresAt?: number
}

export interface TraeState {
  logged?: TraeAccountView[]
  activeUid?: string | null
  regionId?: string
  authFailedUid?: string | null
  hasAccessToken?: boolean
}

interface TraeSlice {
  state: TraeState | null
  loading: boolean
  error: string | null
  load: () => Promise<void>
}

export const useTrae = create<TraeSlice>((set) => ({
  state: null,
  loading: false,
  error: null,
  async load() {
    set({ loading: true })
    try {
      const state = await call<TraeState>('trae_state')
      set({ state, loading: false, error: null })
    } catch (e) {
      set({ state: null, loading: false, error: (e as Error).message })
    }
  },
}))

/// 登录链接：复制 + 打开浏览器（§2.2 startTraeLogin 等值；桌面版把回调收敛为「粘贴链接」）
export async function traeLoginStart(region: string): Promise<void> {
  try {
    const url = await call<string>('trae_login_url', { region })
    toast('登录链接已复制')
    openExternal(url)
  } catch (e) {
    toast((e as Error).message)
  }
}

/// 粘贴回调链接换取 token（§2.2 promptPasteCallback）
export async function traeLoginSubmit(url: string): Promise<boolean> {
  if (!url.trim()) {
    toast('没有粘贴内容，请重新打开本窗口')
    return false
  }
  toast('正在换取 token…')
  try {
    const r = await call<{ ok: boolean; name?: string; error?: string }>('trae_login_submit', { url })
    if (r.ok) toast(`登录成功：${r.name ?? ''}`)
    else toast(`登录失败：${r.error ?? '未知原因'}`)
    await useTrae.getState().load()
    return r.ok
  } catch (e) {
    toast(`登录失败：${(e as Error).message}`)
    return false
  }
}

export async function traeSwitch(uid: string): Promise<void> {
  try {
    await call('trae_switch', { uid })
    toast(`已切换到 ${uid}`)
    await useTrae.getState().load()
  } catch (e) {
    toast((e as Error).message)
  }
}

export async function traeRemove(uid: string, label: string): Promise<void> {
  try {
    await call('trae_remove', { uid })
    toast('已删除')
    await useTrae.getState().load()
  } catch (e) {
    toast((e as Error).message)
    return
  }
  void label
}

/// 概览页 Trae 卡的当前账号（未登录返回 null）
export function activeTraeAccount(state: TraeState | null): TraeAccountView | null {
  if (!state) return null
  const list = state.logged ?? []
  if (list.length === 0) return null
  return list.find((a) => a.uid && a.uid === state.activeUid) ?? list[0]
}

/// Token 到期文案（§2.1：`Token 到期: ${formatExp(expiresAt)}`，格式 `yyyy-MM-dd HH:mm（剩 x.x 天 / 即将过期）`）
export function formatExp(expiresAtSec: number, now: number = Date.now()): string {
  const d = new Date(expiresAtSec * 1000)
  const p = (n: number) => String(n).padStart(2, '0')
  const stamp = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
  const days = (expiresAtSec * 1000 - now) / 86400000
  return `${stamp}（${days < 1 ? '即将过期' : `剩 ${days.toFixed(1)} 天`}）`
}
