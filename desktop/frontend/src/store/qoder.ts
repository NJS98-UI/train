// qoder 切片（§8.2）：与 trae 切片同构，字段按 design.md §Data Models 的 QCred（camelCase）。
import { create } from 'zustand'
import { call } from '../ipc'
import { toast } from './toast'
import { openExternal } from '../ipc'

export interface QoderAccountView {
  uid?: string
  name?: string
  email?: string
  mode?: string
}

export interface QoderState {
  logged?: QoderAccountView[]
  activeUid?: string | null
  hasJobToken?: boolean
}

interface QoderSlice {
  state: QoderState | null
  loading: boolean
  error: string | null
  load: () => Promise<void>
}

export const useQoder = create<QoderSlice>((set) => ({
  state: null,
  loading: false,
  error: null,
  async load() {
    set({ loading: true })
    try {
      const state = await call<QoderState>('qoder_state')
      set({ state, loading: false, error: null })
    } catch (e) {
      set({ state: null, loading: false, error: (e as Error).message })
    }
  },
}))

/// 账号区扩展动作（§2.2 Qoder 卡）
export async function qoderLoginGuide(): Promise<void> {
  openExternal('https://qoder.com/account/integrations')
}

export async function qoderLoginPat(pat: string, mode: string): Promise<boolean> {
  if (!pat.trim()) {
    toast('没有粘贴内容')
    return false
  }
  toast('正在换取 jobToken…')
  try {
    const r = await call<{ ok: boolean; name?: string; error?: string }>('qoder_login_pat', { pat, mode })
    if (r.ok) toast(`登录成功：${r.name ?? ''}`)
    else toast(`登录失败：${r.error ?? '未知原因'}`)
    await useQoder.getState().load()
    return r.ok
  } catch (e) {
    toast(`登录失败：${(e as Error).message}`)
    return false
  }
}

export async function qoderSwitch(uid: string): Promise<void> {
  try {
    await call('qoder_switch', { uid })
    toast(`已切换到 Qoder ${uid}`)
    await useQoder.getState().load()
  } catch (e) {
    toast((e as Error).message)
  }
}

export async function qoderRemove(uid: string): Promise<void> {
  try {
    await call('qoder_remove', { uid })
    toast('已删除')
    await useQoder.getState().load()
  } catch (e) {
    toast((e as Error).message)
  }
}

export async function qoderFingerprintGet(uid: string): Promise<Record<string, string> | null> {
  try {
    return await call<Record<string, string> | null>('qoder_fingerprint_get', { uid })
  } catch {
    return null
  }
}

export async function qoderFingerprintSet(uid: string, fp: { id?: string; token?: string; machineType?: string }): Promise<true | string> {
  if (!fp.id?.trim() || !fp.token?.trim()) return '两项都留空 = 用本机派生；要覆盖就两项都填'
  try {
    await call('qoder_fingerprint_set', { uid, fingerprint: fp })
    return true
  } catch (e) {
    return (e as Error).message
  }
}

export async function qoderFingerprintClear(uid: string): Promise<void> {
  try {
    await call('qoder_fingerprint_clear', { uid })
    toast('已清除，改回本机派生身份')
  } catch (e) {
    toast((e as Error).message)
  }
}

/// 区域文案（§2.2 第 3 项）：mode == MODE_GLOBAL ? 国际版 : 国内版
export function qoderModeLabel(mode: string | null | undefined): string {
  return mode === 'cn' ? '国内版' : '国际版'
}
