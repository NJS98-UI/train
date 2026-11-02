// uiStyle 切片（§8.2）：styleId/modeId/isNight + 派生 CSS 变量；写入即整树重渲染（对应 recreate()），
// 但只更新 :root 变量，不做组件 remount。
import { create } from 'zustand'
import { DEFAULT_STYLE_ID, type ModeId, MODES, findStyle, resolveIsNight } from '../theme/styles'
import { useSettings, updateStyleField } from './settings'

interface UiStyleState {
  styleId: string
  mode: ModeId
  prefersDark: boolean
  isNight: boolean
  /// 从 config.style 同步（App 启动 / config_update 回写后调用）
  syncFromConfig: (style: { styleId?: string | null; mode?: string | null } | null | undefined) => void
  setStyle: (styleId: string) => Promise<void>
  setMode: (mode: ModeId) => Promise<void>
  setPrefersDark: (prefersDark: boolean) => void
  styleLabel: () => string
}

function derive(mode: ModeId, prefersDark: boolean): boolean {
  return resolveIsNight(mode, prefersDark)
}

/// mode 字段容错：读到已删除/非法 id 退回默认（对齐 §4.1 init 的回落规则）
function normalizeMode(mode: string | null | undefined): ModeId {
  return MODES.some((m) => m.id === mode) ? (mode as ModeId) : 'system'
}

export const useUiStyle = create<UiStyleState>((set, get) => ({
  styleId: DEFAULT_STYLE_ID,
  mode: 'system',
  prefersDark: false,
  isNight: false,
  syncFromConfig(style) {
    const styleId = style?.styleId ? findStyle(style.styleId).id : get().styleId
    const mode = normalizeMode(style?.mode)
    set({ styleId, mode, isNight: derive(mode, get().prefersDark) })
  },
  async setStyle(styleId) {
    const next = findStyle(styleId).id
    if (next === get().styleId) return // 同 id 直接 return（§2.6 switchStyle）
    set({ styleId: next })
    await updateStyleField(next, get().mode)
  },
  async setMode(mode) {
    const next = normalizeMode(mode)
    set({ mode: next, isNight: derive(next, get().prefersDark) })
    await updateStyleField(get().styleId, next)
  },
  setPrefersDark(prefersDark) {
    set({ prefersDark, isNight: derive(get().mode, prefersDark) })
  },
  styleLabel() {
    return findStyle(get().styleId).label
  },
}))

/// 供页面读取当前风格形态（色样圆角等）
export function currentShape() {
  const s = useUiStyle.getState().styleId
  return findStyle(s).shape
}

/// 供组件在主题变化时读取（非 hook，仅兜底使用）
export function configStyleNow(): { styleId: string; mode: ModeId } {
  const st = useUiStyle.getState()
  const cfg = useSettings.getState().config
  return { styleId: st.styleId ?? cfg.style.styleId, mode: st.mode }
}
