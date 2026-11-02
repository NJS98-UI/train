// 徽章原子（§8.1）：Dot / Chip(ok|warn|info) / StatusChip(tone, dotOn)
// chip 最小高 32px + 横向 pad 12px；圆点 9px；实心 = tone 色底、空心 = tone 描边 + surface 底（§4.3 / §8.3）。
import type { CSSProperties, ReactNode } from 'react'
import { cssVar, type ColorKey, type Tone } from '../theme/styles'

const TONE_FG: Record<Tone, ColorKey> = {
  primary: 'primary',
  secondary: 'secondary',
  success: 'success',
  warning: 'warning',
  error: 'error',
}

const TONE_CONTAINER: Record<Tone, ColorKey> = {
  primary: 'primaryContainer',
  secondary: 'surfaceVariant',
  success: 'successContainer',
  warning: 'warningContainer',
  error: 'errorContainer',
}

export type ChipKind = 'ok' | 'warn' | 'info'

const CHIP_TONE: Record<ChipKind, Tone> = { ok: 'success', warn: 'warning', info: 'primary' }

/// Dot(tone, hollow)：9px 状态圆点
export function Dot({ tone = 'success', hollow = false, style }: { tone?: Tone; hollow?: boolean; style?: CSSProperties }) {
  const s: CSSProperties = { ...style }
  if (hollow) {
    s.border = `2px solid ${cssVar(TONE_FG[tone])}`
    s.background = cssVar('surface')
  } else {
    s.background = cssVar(TONE_FG[tone])
  }
  return <span className="tw-dot" style={s} />
}

/// chipOk / chipWarn / chipInfo：容器底色 + tone 前景色，FS_CHIP
export function Chip({ kind = 'info', children }: { kind?: ChipKind; children: ReactNode }) {
  const tone = CHIP_TONE[kind]
  const style: CSSProperties = { background: cssVar(TONE_CONTAINER[tone]), color: cssVar(TONE_FG[tone]) }
  return <span className="tw-chip" style={style}>{children}</span>
}

/// statusChip(label, tone, on)：<tone>_container 底 + tone 字 + 圆点（on = 实心，off = 空心）
export function StatusChip({ tone = 'success', dotOn = true, children }: { tone?: Tone; dotOn?: boolean; children: ReactNode }) {
  const style: CSSProperties = { background: cssVar(TONE_CONTAINER[tone]), color: cssVar(TONE_FG[tone]) }
  return (
    <span className="tw-chip" style={style}>
      <Dot tone={tone} hollow={!dotOn} />
      {children}
    </span>
  )
}
