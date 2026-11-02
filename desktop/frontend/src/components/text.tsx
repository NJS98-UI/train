// 文本原子（§8.1 Text/Mono/Caption/LabelValue）：字号一律取 --fs-* token（§4.3 FS 表），
// 颜色一律取语义色 var(--tw-*)，禁止硬编码。
import type { CSSProperties, ReactNode } from 'react'
import type { ColorKey } from '../theme/styles'
import { cssVar } from '../theme/styles'

export type FsName = 'title' | 'section' | 'cardTitle' | 'metric' | 'body' | 'value' | 'caption' | 'chip'

const FS_CLASS: Record<FsName, string> = {
  title: 'tw-fs-title',
  section: 'tw-fs-section',
  cardTitle: 'tw-fs-card-title',
  metric: 'tw-fs-metric',
  body: 'tw-fs-body',
  value: 'tw-fs-value',
  caption: 'tw-fs-caption',
  chip: 'tw-fs-chip',
}

function colorValue(color: ColorKey | string | undefined): string | undefined {
  if (!color) return undefined
  return color.startsWith('--') || color.startsWith('var(') ? color : cssVar(color as ColorKey)
}

interface TextProps {
  children: ReactNode
  fs?: FsName
  bold?: boolean
  color?: ColorKey | 'onSurface' | string
  align?: 'left' | 'center' | 'right'
  maxLines?: number
  selectable?: boolean
  grow?: boolean
  className?: string
  style?: CSSProperties
  onClick?: () => void
}

export function Text({ children, fs = 'body', bold, color, align, maxLines, selectable, grow, className, style, onClick }: TextProps) {
  const merged: CSSProperties = { ...style }
  if (color) merged.color = colorValue(color)
  if (align) merged.textAlign = align
  if (grow) merged.flex = '1 1 auto'
  if (maxLines === 1) merged.overflow = 'hidden'
  if (maxLines) {
    merged.display = '-webkit-box'
    merged.WebkitLineClamp = maxLines
    merged.WebkitBoxOrient = 'vertical'
    merged.overflow = 'hidden'
  }
  const cls = ['tw-text', FS_CLASS[fs], bold ? 'tw-bold' : '', maxLines === 1 ? 'tw-ellipsis' : '', selectable ? 'tw-selectable' : '', className ?? '']
    .filter(Boolean)
    .join(' ')
  return (
    <span className={cls} style={merged} onClick={onClick}>
      {children}
    </span>
  )
}

interface MonoProps {
  children: ReactNode
  fs?: FsName
  bold?: boolean
  color?: string
  grow?: boolean
  selectable?: boolean
  className?: string
  style?: CSSProperties
}

export function Mono({ children, fs = 'body', bold, color, grow, selectable, className, style }: MonoProps) {
  const merged: CSSProperties = { ...style }
  if (color) merged.color = colorValue(color)
  if (grow) merged.flex = '1 1 auto'
  const cls = ['tw-mono', FS_CLASS[fs], bold ? 'tw-bold' : '', selectable ? 'tw-selectable' : '', className ?? ''].filter(Boolean).join(' ')
  return <span className={cls} style={merged}>{children}</span>
}

/// caption(...)：说明文字（FS_CAPTION=13）
export function Caption({ children, className, style, grow }: { children: ReactNode; className?: string; style?: CSSProperties; grow?: boolean }) {
  return (
    <Text fs="caption" color="onSurfaceVariant" className={className} style={style} grow={grow}>
      {children}
    </Text>
  )
}

/// labelValue(...)：标签列 minWidth 96px + 值可选中复制（§2.2）
export function LabelValue({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="tw-label-value">
      <span className="tw-label-value__label">{label}</span>
      <span className="tw-label-value__value tw-selectable">{value}</span>
    </div>
  )
}
