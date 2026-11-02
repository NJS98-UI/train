// AppStyle 对应体：语义色键 → 运行期查表（ui-keepalive.md §4.1）。
// 色值逐格抄自 ui-keepalive.md §4.2（4 风格 × light/dark × 19 键，顺序即「外观风格」页展示顺序，首个 indigo 为默认）。
// 组件侧只允许 var(--tw-*)，禁止硬编码 hex。

export type ColorKey =
  | 'primary'
  | 'primaryContainer'
  | 'onPrimary'
  | 'onPrimaryContainer'
  | 'secondary'
  | 'background'
  | 'surface'
  | 'surfaceContainer'
  | 'surfaceVariant'
  | 'onSurface'
  | 'onSurfaceVariant'
  | 'outline'
  | 'outlineVariant'
  | 'success'
  | 'successContainer'
  | 'warning'
  | 'warningContainer'
  | 'error'
  | 'errorContainer'

/// KEYS 顺序同 AppStyle.kt（§4.1），共 19 个
export const COLOR_KEYS: readonly ColorKey[] = [
  'primary',
  'primaryContainer',
  'onPrimary',
  'onPrimaryContainer',
  'secondary',
  'background',
  'surface',
  'surfaceContainer',
  'surfaceVariant',
  'onSurface',
  'onSurfaceVariant',
  'outline',
  'outlineVariant',
  'success',
  'successContainer',
  'warning',
  'warningContainer',
  'error',
  'errorContainer',
] as const

export type Palette = Record<ColorKey, string>

export type Tone = 'primary' | 'success' | 'warning' | 'error' | 'secondary'

export interface Shape {
  cardRadius: number
  controlRadius: number
  pillRadius: number
  gap: number
  pagePadH: number
  pagePadTop: number
  pagePadBottom: number
  cardPad: number
  barPadTop: number
  barPadBottom: number
  contentMaxDp: number
  elevation: number
}

export interface StyleDef {
  id: string
  label: string
  note: string
  shape: Shape
  light: Palette
  dark: Palette
}

export type ModeId = 'light' | 'dark' | 'system'

export const MODES: { id: ModeId; label: string; note: string }[] = [
  { id: 'system', label: '跟随系统', note: '系统开夜间模式时自动变深' },
  { id: 'light', label: '浅色', note: '始终使用浅色' },
  { id: 'dark', label: '深色', note: '始终使用深色' },
]

// ① 靛蓝（§4.2 ① 形态行 + 色表 light/dark 两列）
const INDIGO: StyleDef = {
  id: 'indigo',
  label: '靛蓝',
  note: '经典靛蓝 · 中圆角 · 标准间距',
  shape: {
    cardRadius: 16,
    controlRadius: 12,
    pillRadius: 100,
    gap: 12,
    pagePadH: 20,
    pagePadTop: 16,
    pagePadBottom: 24,
    cardPad: 16,
    barPadTop: 14,
    barPadBottom: 12,
    contentMaxDp: 640,
    elevation: 1,
  },
  light: {
    primary: '#4F46E5',
    primaryContainer: '#E6E7FF',
    onPrimary: '#FFFFFF',
    onPrimaryContainer: '#17115B',
    secondary: '#2563A9',
    background: '#F7F7FC',
    surface: '#FFFFFF',
    surfaceContainer: '#F0F0F8',
    surfaceVariant: '#E7E7F0',
    onSurface: '#1B1B22',
    onSurfaceVariant: '#5F5F6B',
    outline: '#777783',
    outlineVariant: '#C7C7D1',
    success: '#16794B',
    successContainer: '#D7F6E5',
    warning: '#8A5100',
    warningContainer: '#FFE9C2',
    error: '#BA1A1A',
    errorContainer: '#FFDAD6',
  },
  dark: {
    primary: '#BEC2FF',
    primaryContainer: '#34358B',
    onPrimary: '#20205F',
    onPrimaryContainer: '#E1E0FF',
    secondary: '#8AB8E8',
    background: '#101016',
    surface: '#19191F',
    surfaceContainer: '#23232B',
    surfaceVariant: '#2E2E38',
    onSurface: '#E5E1EA',
    onSurfaceVariant: '#B5B3C0',
    outline: '#8B8B97',
    outlineVariant: '#3A3A44',
    success: '#66E3A0',
    successContainer: '#1E4A34',
    warning: '#FFD08A',
    warningContainer: '#5A3D17',
    error: '#FFB4A8',
    errorContainer: '#5A2B2B',
  },
}

// ② 石墨（§4.2 ②）
const GRAPHITE: StyleDef = {
  id: 'graphite',
  label: '石墨',
  note: '冷灰中性 · 方正小圆角 · 紧凑间距',
  shape: {
    cardRadius: 6,
    controlRadius: 6,
    pillRadius: 8,
    gap: 10,
    pagePadH: 16,
    pagePadTop: 14,
    pagePadBottom: 18,
    cardPad: 14,
    barPadTop: 12,
    barPadBottom: 10,
    contentMaxDp: 680,
    elevation: 0,
  },
  light: {
    primary: '#3F4754',
    primaryContainer: '#DDE2EA',
    onPrimary: '#FFFFFF',
    onPrimaryContainer: '#191C22',
    secondary: '#4A5568',
    background: '#ECEDEF',
    surface: '#FFFFFF',
    surfaceContainer: '#F4F5F7',
    surfaceVariant: '#E2E4E8',
    onSurface: '#16181D',
    onSurfaceVariant: '#55595F',
    outline: '#6E737B',
    outlineVariant: '#C6C9CE',
    success: '#14663F',
    successContainer: '#D2EEDD',
    warning: '#7A4700',
    warningContainer: '#FBE3BC',
    error: '#A81B1B',
    errorContainer: '#FBDAD8',
  },
  dark: {
    primary: '#B8C2D0',
    primaryContainer: '#2A313C',
    onPrimary: '#1A2028',
    onPrimaryContainer: '#DCE2EA',
    secondary: '#9AA6B5',
    background: '#101215',
    surface: '#17191D',
    surfaceContainer: '#1F2227',
    surfaceVariant: '#272B31',
    onSurface: '#E3E5E9',
    onSurfaceVariant: '#A9AEB6',
    outline: '#868C95',
    outlineVariant: '#34383E',
    success: '#7FD9A8',
    successContainer: '#12321F',
    warning: '#F5C87A',
    warningContainer: '#3A2A06',
    error: '#FFB3AC',
    errorContainer: '#521413',
  },
}

// ③ 苔绿（§4.2 ③）
const MOSS: StyleDef = {
  id: 'moss',
  label: '苔绿',
  note: '自然苔绿 · 大圆角 · 舒展间距',
  shape: {
    cardRadius: 24,
    controlRadius: 18,
    pillRadius: 100,
    gap: 16,
    pagePadH: 24,
    pagePadTop: 20,
    pagePadBottom: 28,
    cardPad: 20,
    barPadTop: 16,
    barPadBottom: 14,
    contentMaxDp: 600,
    elevation: 2,
  },
  light: {
    primary: '#2F6C46',
    primaryContainer: '#D6EFDD',
    onPrimary: '#FFFFFF',
    onPrimaryContainer: '#0E2416',
    secondary: '#4A7C59',
    background: '#F4F7F2',
    surface: '#FFFFFF',
    surfaceContainer: '#EDF3EA',
    surfaceVariant: '#DFE9DD',
    onSurface: '#131A15',
    onSurfaceVariant: '#4F5B52',
    outline: '#6C7A6F',
    outlineVariant: '#C3CFC2',
    success: '#1A6B3C',
    successContainer: '#D9F2E1',
    warning: '#86530A',
    warningContainer: '#FCE8C3',
    error: '#A8261F',
    errorContainer: '#FBDCD7',
  },
  dark: {
    primary: '#8FD5A6',
    primaryContainer: '#1D3A28',
    onPrimary: '#0D2A18',
    onPrimaryContainer: '#CDEBD8',
    secondary: '#A9C9AF',
    background: '#0F1410',
    surface: '#161C17',
    surfaceContainer: '#1E251F',
    surfaceVariant: '#262E27',
    onSurface: '#E1E8E1',
    onSurfaceVariant: '#AAB7AC',
    outline: '#8A978A',
    outlineVariant: '#333B33',
    success: '#8FE0AE',
    successContainer: '#12331F',
    warning: '#F7CA80',
    warningContainer: '#3B2B06',
    error: '#FFB4AC',
    errorContainer: '#53140F',
  },
}

// ④ 落日（§4.2 ④）
const SUNSET: StyleDef = {
  id: 'sunset',
  label: '落日',
  note: '暖橙落日 · 大圆角 · 宽松内边距',
  shape: {
    cardRadius: 20,
    controlRadius: 14,
    pillRadius: 100,
    gap: 12,
    pagePadH: 20,
    pagePadTop: 18,
    pagePadBottom: 24,
    cardPad: 18,
    barPadTop: 14,
    barPadBottom: 12,
    contentMaxDp: 640,
    elevation: 1,
  },
  light: {
    primary: '#A8431A',
    primaryContainer: '#FFE2D4',
    onPrimary: '#FFFFFF',
    onPrimaryContainer: '#3A1406',
    secondary: '#8A5A2B',
    background: '#FDF7F3',
    surface: '#FFFFFF',
    surfaceContainer: '#F8EFE9',
    surfaceVariant: '#EFE2DA',
    onSurface: '#211711',
    onSurfaceVariant: '#5C4A3E',
    outline: '#7D6A5D',
    outlineVariant: '#D6C5B8',
    success: '#1A6B3C',
    successContainer: '#D9F2E1',
    warning: '#8A5100',
    warningContainer: '#FFE9C2',
    error: '#B3261E',
    errorContainer: '#FFDAD5',
  },
  dark: {
    primary: '#FFB59B',
    primaryContainer: '#6E2607',
    onPrimary: '#571D06',
    onPrimaryContainer: '#FFDBCB',
    secondary: '#E0B58C',
    background: '#17110D',
    surface: '#1F1813',
    surfaceContainer: '#291F18',
    surfaceVariant: '#33261D',
    onSurface: '#EFE1D9',
    onSurfaceVariant: '#C4B0A3',
    outline: '#9C8A7C',
    outlineVariant: '#45372C',
    success: '#8FE0AE',
    successContainer: '#12331F',
    warning: '#F7CA80',
    warningContainer: '#3B2B06',
    error: '#FFB4AB',
    errorContainer: '#5C1412',
  },
}

/// 顺序即展示顺序，首个为默认（§4.2 标题）
export const STYLES: readonly StyleDef[] = [INDIGO, GRAPHITE, MOSS, SUNSET]

export const DEFAULT_STYLE_ID = INDIGO.id

export function findStyle(id: string | null | undefined): StyleDef {
  return STYLES.find((s) => s.id === id) ?? STYLES[0]
}

/// §4.3 不随风格变的字号 token（sp 数值直作 px）
export const FS = {
  title: 22,
  section: 14,
  cardTitle: 17,
  metric: 24,
  body: 15,
  value: 14,
  caption: 13,
  chip: 12,
} as const

export type FsName = keyof typeof FS

/// 其它固定尺寸（§4.3 表，非风格变量）
export const SIZES = {
  navItemHeight: 64,
  chipMinHeight: 32,
  chipPadH: 12,
  dotDiameter: 9,
  dividerHeight: 1,
  buttonMinHeight: 48,
  buttonPadH: 18,
  linkMinSize: 48,
  linkPadH: 14,
  labelMinWidth: 96,
  shareBarHeight: 6,
  inputPadV: 14,
  inputPadH: 12,
  swatchSize: 48,
  swatchPillW: 24,
  swatchPillH: 7,
  radioOuter: 22,
  radioInner: 8,
  pickerRowHeight: 56,
  pickerVisibleRows: 5,
  styleRowMinHeight: 56,
  modeRowMinHeight: 52,
  maskedKeyDots: 24,
  port: 8787,
} as const

/// MASKED_KEY = `sk-` + 24 个 U+2022（§2.1 第 3 卡 / §4.3）
export const MASKED_KEY = `sk-${'\u2022'.repeat(SIZES.maskedKeyDots)}`

export function keyToCssVar(key: ColorKey): string {
  return `--tw-${key.replace(/[A-Z]/g, (m) => m.toLowerCase().replace(/^/, '-'))}`
}

export function cssVar(key: ColorKey): string {
  return `var(${keyToCssVar(key)})`
}

function shapeVars(shape: Shape): Record<string, string> {
  return {
    '--radius-card': `${shape.cardRadius}px`,
    '--radius-control': `${shape.controlRadius}px`,
    // pill 建议 min(pillRadius, 9999px)；graphite 的 8 原样数值（§8.3）
    '--radius-pill': `${Math.min(shape.pillRadius, 9999)}px`,
    '--gap': `${shape.gap}px`,
    '--page-pad-h': `${shape.pagePadH}px`,
    '--page-pad-top': `${shape.pagePadTop}px`,
    '--page-pad-bottom': `${shape.pagePadBottom}px`,
    '--card-pad': `${shape.cardPad}px`,
    '--bar-pad-top': `${shape.barPadTop}px`,
    '--bar-pad-bottom': `${shape.barPadBottom}px`,
    '--content-max': `${shape.contentMaxDp}px`,
    '--card-elevation': `${shape.elevation}`,
    // box-shadow: 0 {n}px {2n}px rgba(...)，仅 NORMAL 卡使用（§8.3）
    '--card-shadow': shape.elevation <= 0 ? 'none' : `0 ${shape.elevation}px ${shape.elevation * 2}px rgba(0,0,0,0.18)`,
  }
}

function fsVars(): Record<string, string> {
  return {
    '--fs-title': `${FS.title}px`,
    '--fs-section': `${FS.section}px`,
    '--fs-card-title': `${FS.cardTitle}px`,
    '--fs-metric': `${FS.metric}px`,
    '--fs-body': `${FS.body}px`,
    '--fs-value': `${FS.value}px`,
    '--fs-caption': `${FS.caption}px`,
    '--fs-chip': `${FS.chip}px`,
  }
}

export function systemPrefersDark(): boolean {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia('(prefers-color-scheme: dark)').matches
    : false
}

/// 由 {mode, 系统偏好} 判定是否夜间（等价 applyNightOverride + refreshNight，§4.1）
export function resolveIsNight(mode: ModeId, prefersDark: boolean = systemPrefersDark()): boolean {
  if (mode === 'light') return false
  if (mode === 'dark') return true
  return prefersDark
}

export function paletteOf(styleId: string, isNight: boolean): Palette {
  const style = findStyle(styleId)
  return isNight ? style.dark : style.light
}

/// 把 {styleId, mode} 解析成完整 CSS 变量集（19 键 → --tw-*，§8.3 迁移表）
export function buildCssVars(styleId: string, mode: ModeId, prefersDark?: boolean): Record<string, string> {
  const style = findStyle(styleId)
  const isNight = resolveIsNight(mode, prefersDark)
  const palette = isNight ? style.dark : style.light
  const colorVars: Record<string, string> = {}
  for (const key of COLOR_KEYS) {
    colorVars[keyToCssVar(key)] = palette[key]
  }
  return { ...colorVars, ...shapeVars(style.shape), ...fsVars() }
}

export function applyTheme(el: HTMLElement, styleId: string, mode: ModeId, prefersDark?: boolean): boolean {
  const vars = buildCssVars(styleId, mode, prefersDark)
  for (const [name, value] of Object.entries(vars)) el.style.setProperty(name, value)
  el.setAttribute('data-style', findStyle(styleId).id)
  el.setAttribute('data-mode', resolveIsNight(mode, prefersDark) ? 'dark' : 'light')
  el.style.colorScheme = resolveIsNight(mode, prefersDark) ? 'dark' : 'light'
  return resolveIsNight(mode, prefersDark)
}

// ---- selfCheck（对齐 AppStyle.selfCheck，§4.1 / 验收 §9.9）----

/// CONTRAST_PAIRS（§4.1，前景/背景 组合，要求 ≥ 4.5）
export const CONTRAST_PAIRS: [ColorKey, ColorKey][] = [
  ['onSurface', 'surface'],
  ['onSurface', 'background'],
  ['onSurface', 'primaryContainer'],
  ['onSurface', 'surfaceContainer'],
  ['onSurfaceVariant', 'surface'],
  ['onSurfaceVariant', 'background'],
  ['onSurfaceVariant', 'primaryContainer'],
  ['onSurfaceVariant', 'surfaceContainer'],
  ['onPrimary', 'primary'],
  ['primary', 'surface'],
  ['primary', 'primaryContainer'],
  ['primary', 'surfaceContainer'],
  ['onPrimaryContainer', 'primaryContainer'],
  ['success', 'successContainer'],
  ['warning', 'warningContainer'],
  ['error', 'errorContainer'],
]

function channel(v: number): number {
  const c = v / 255
  return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4)
}

export function luminance(hex: string): number {
  const r = parseInt(hex.slice(1, 3), 16)
  const g = parseInt(hex.slice(3, 5), 16)
  const b = parseInt(hex.slice(5, 7), 16)
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

export function contrastRatio(fg: string, bg: string): number {
  const a = luminance(fg)
  const b = luminance(bg)
  const [hi, lo] = a >= b ? [a, b] : [b, a]
  return (hi + 0.05) / (lo + 0.05)
}

export interface StyleIssue {
  style: string
  mode: 'light' | 'dark'
  detail: string
}

/// 返回全部违规；空数组即通过（19 键齐全 / #RRGGBB / 对比度 ≥4.5 / 形态合法）
export function selfCheck(): StyleIssue[] {
  const issues: StyleIssue[] = []
  const ids = new Set<string>()
  if (STYLES.length < 3) issues.push({ style: '-', mode: 'light', detail: '风格套数不足 3' })
  for (const style of STYLES) {
    if (ids.has(style.id)) issues.push({ style: style.id, mode: 'light', detail: '风格 id 重复' })
    ids.add(style.id)
    const s = style.shape
    if (!(s.cardRadius > 0 && s.controlRadius > 0 && s.pillRadius > 0)) {
      issues.push({ style: style.id, mode: 'light', detail: '圆角必须全 > 0' })
    }
    if (!(s.cardPad > 0 && s.pagePadH > 0 && s.gap >= 0)) {
      issues.push({ style: style.id, mode: 'light', detail: 'cardPad>0 && pagePadH>0 && gap>=0 不满足' })
    }
    if (!(s.contentMaxDp >= 480 && s.contentMaxDp <= 900)) {
      issues.push({ style: style.id, mode: 'light', detail: `contentMaxDp=${s.contentMaxDp} 超出 480..900` })
    }
    for (const mode of ['light', 'dark'] as const) {
      const palette = mode === 'light' ? style.light : style.dark
      for (const key of COLOR_KEYS) {
        const hex = palette[key]
        if (typeof hex !== 'string' || !/^#[0-9A-Fa-f]{6}$/.test(hex)) {
          issues.push({ style: style.id, mode, detail: `${key} 缺失或非 #RRGGBB：${String(hex)}` })
        }
      }
      for (const [fg, bg] of CONTRAST_PAIRS) {
        const f = palette[fg]
        const b = palette[bg]
        if (!f || !b) continue
        const ratio = contrastRatio(f, b)
        if (ratio < 4.5) {
          issues.push({ style: style.id, mode, detail: `对比度 ${fg}/${bg} = ${ratio.toFixed(2)} < 4.5` })
        }
      }
    }
  }
  return issues
}
