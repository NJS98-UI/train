// 交互原子（§8.1）：ActionButton / LinkPill / AccountBar / SearchInput / ShareBar
// 触控下限 min-height 48px（§8.3）；已签/禁用态胶囊允许 32px（仅不可点或弱化）。
import type { CSSProperties, ReactNode } from 'react'
import { cssVar } from '../theme/styles'

export type ButtonVariant = 'filled' | 'outlined' | 'full' | 'danger'
export type ButtonState = 'done' | 'muted'

interface ActionButtonProps {
  children: ReactNode
  variant?: ButtonVariant
  state?: ButtonState
  disabled?: boolean
  onClick?: () => void
  className?: string
  style?: CSSProperties
}

/// action(...)：主按钮。filled/outlined/full/danger + done(✓ success 胶囊)/muted(surface_variant 胶囊) 两态外观
export function ActionButton({ children, variant = 'filled', state, disabled, onClick, className, style }: ActionButtonProps) {
  const cls = ['tw-btn', `tw-btn--${variant}`, state ? `tw-btn--${state}` : '', disabled ? 'tw-btn--disabled' : '', className ?? '']
    .filter(Boolean)
    .join(' ')
  return (
    <button type="button" className={cls} style={style} disabled={disabled} onClick={onClick}>
      {children}
    </button>
  )
}

/// linkAction(...)：胶囊文字按钮（13px 粗体 primary，min 48×48；overContainer 时底色换 surface）
export function LinkPill({
  children,
  onClick,
  overContainer,
  disabled,
  tone = 'primary',
  className,
}: {
  children: ReactNode
  onClick?: () => void
  overContainer?: boolean
  disabled?: boolean
  tone?: 'primary' | 'error' | 'onSurfaceVariant'
  className?: string
}) {
  const style: CSSProperties = {}
  if (overContainer) style.background = cssVar('surface')
  const colorKey = tone === 'primary' ? 'primary' : tone === 'error' ? 'error' : 'onSurfaceVariant'
  return (
    <button
      type="button"
      className={`tw-link ${disabled ? 'tw-btn--disabled' : ''} ${className ?? ''}`}
      style={{ color: cssVar(colorKey), ...style }}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </button>
  )
}

/// accountBar(...)：整行可点条（左标题 14 bold + 右 ▾ 17）
export function AccountBar({ label, onClick }: { label: string; onClick?: () => void }) {
  return (
    <button type="button" className="tw-accountbar" onClick={onClick}>
      <span className="tw-accountbar__label">{label}</span>
      <span className="tw-accountbar__caret">▾</span>
    </button>
  )
}

/// searchInput(...)：单行输入，surface + outline_variant 描边（§2.3）
export function SearchInput({
  value,
  placeholder,
  onChange,
}: {
  value: string
  placeholder: string
  onChange: (v: string) => void
}) {
  return <input className="tw-search" type="text" value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} />
}

/// shareBar(value, max)：6px 高占比条，surface_variant 底 + primary 前景，比例 = value / 最大组（§2.4）
export function ShareBar({ value, max }: { value: number; max: number }) {
  const ratio = max > 0 ? Math.max(0, Math.min(1, value / max)) : 0
  return (
    <div className="tw-sharebar">
      <div className="tw-sharebar__fill" style={{ width: `${ratio * 100}%` }} />
    </div>
  )
}
