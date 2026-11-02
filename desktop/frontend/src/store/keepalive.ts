// keepalive 切片：desktop_checks → CheckItem[6]{title,ok,why,autoFixable}；
// 开关项（autostart / close_to_tray）读写走 config_get/config_update。
// 一键配置沿用 Android 规则：一次只引导一项（多弹窗会互相顶掉，§2.5 第 2 项）。
import { create } from 'zustand'
import { call } from '../ipc'
import { useSettings } from './settings'
import { useService } from './service'
import { toast } from './toast'

export interface CheckItem {
  title: string
  ok: boolean
  why: string
  autoFixable: boolean
}

interface KeepAliveSlice {
  checks: CheckItem[]
  pending: number
  autoFixablePending: number
  loading: boolean
  loaded: boolean
  error: string | null
  load: () => Promise<void>
  setCloseToTray: (on: boolean) => Promise<void>
  setAutostart: (on: boolean) => Promise<void>
  /// 针对单项执行可自动处理的修复
  fixItem: (item: CheckItem) => Promise<void>
  /// 「开始自动配置」：只处理第一个未就绪且 autoFixable 的项
  autoConfigureOne: () => Promise<void>
}

const CLOSE_TO_TRAY_HINT = '关窗 / 后台'
const AUTOSTART_HINT = '自启'
const SERVICE_HINT = '网关 / 运行 / 端口 / 监听 / 健康'

function matches(title: string, pattern: string): boolean {
  return new RegExp(pattern).test(title)
}

export const useKeepAlive = create<KeepAliveSlice>((set) => ({
  checks: [],
  pending: 0,
  autoFixablePending: 0,
  loading: false,
  loaded: false,
  error: null,

  async load() {
    set({ loading: true })
    try {
      const raw = await call<CheckItem[]>('desktop_checks')
      const list = Array.isArray(raw) ? raw : []
      const pendingList = list.filter((c) => !c.ok)
      set({
        checks: list,
        pending: pendingList.length,
        autoFixablePending: pendingList.filter((c) => c.autoFixable).length,
        loading: false,
        loaded: true,
        error: null,
      })
    } catch (e) {
      set({ checks: [], pending: 0, autoFixablePending: 0, loading: false, loaded: true, error: (e as Error).message })
    }
  },

  async setCloseToTray(on) {
    const ok = await useSettings.getState().patch({ closeToTray: on })
    if (!ok) toast('设置保存失败，请稍后重试')
    if (ok && on) toast('已开启关窗最小化到托盘')
    if (ok && !on) toast('已关闭关窗最小化到托盘')
  },

  async setAutostart(on) {
    const ok = await useSettings.getState().patch({ autostart: on })
    if (!ok) toast('设置保存失败，请稍后重试')
    if (ok && on) toast('已开启开机自启')
    if (ok && !on) toast('已关闭开机自启')
  },

  async fixItem(item) {
    const t = item.title
    if (matches(t, SERVICE_HINT) && !useService.getState().running) {
      await useService.getState().start()
      await useService.getState().refresh()
      return
    }
    if (matches(t, CLOSE_TO_TRAY_HINT)) {
      await useKeepAlive.getState().setCloseToTray(true)
      return
    }
    if (matches(t, AUTOSTART_HINT)) {
      await useKeepAlive.getState().setAutostart(true)
      return
    }
    // 桌面侧无法自动打开的项，给出与 Android 一致的手动提示文案
    toast('这个页面在本机打不开，请按下方路径手动设置')
  },

  async autoConfigureOne() {
    const target = useKeepAlive.getState().checks.find((c) => !c.ok && c.autoFixable)
    if (!target) {
      toast('能自动配置的项都已就绪')
      return
    }
    await useKeepAlive.getState().fixItem(target)
    // 800ms 后回本页取最新状态（对齐 openAutoItem 的重绘时机）
    setTimeout(() => void useKeepAlive.getState().load(), 800)
  },
}))
