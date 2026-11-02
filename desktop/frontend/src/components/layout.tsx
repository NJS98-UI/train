// 布局原子（§8.1）：ScrollPage / Card(tone) / SectionTitle / Row / Divider
// 尺寸全部来自主题 token（§4.2 形态 + §4.3 固定尺寸）。
import type { CSSProperties, ReactNode } from 'react'

/// scroll{}：pagePad* 内边距 + contentMaxDp 限宽居中，滚动位置由本节点保持（原地更新不重建的关键）
export function ScrollPage({ children }: { children: ReactNode }) {
  return (
    <div className="tw-scroll">
      <div className="tw-page">{children}</div>
    </div>
  )
}

export type CardTone = 'normal' | 'soft' | 'emphasis'

interface CardProps {
  tone?: CardTone
  children: ReactNode
  top?: boolean
  className?: string
  style?: CSSProperties
}

/// 卡片三态（§2 约定）：
/// normal = surface + 1dp outline_variant 描边 + 阴影；soft = surface_container 无描边无阴影；emphasis = primary_container 无描边无阴影
export function Card({ tone = 'normal', children, top = true, className, style }: CardProps) {
  const merged: CSSProperties = { ...style }
  if (!top) merged.marginTop = 0
  return <div className={`tw-card tw-card--${tone} ${className ?? ''}`} style={merged}>{children}</div>
}

/// section(...)：区块标题，FS_SECTION + letterSpacing 0.04em + 上下 18/8 内边距（§4.3）
export function SectionTitle({ children }: { children: ReactNode }) {
  return <div className="tw-section">{children}</div>
}

/// row(...)：横向排布，top 为与上一行的间距（GAP 等值）
export function Row({
  children,
  gap,
  top,
  align = 'center',
  wrap,
  className,
  style,
  onClick,
}: {
  children: ReactNode
  gap?: number
  top?: number
  align?: 'center' | 'stretch' | 'start' | 'end'
  wrap?: boolean
  className?: string
  style?: CSSProperties
  onClick?: () => void
}) {
  const merged: CSSProperties = { ...style, alignItems: align }
  if (gap !== undefined) merged.gap = `${gap}px`
  if (top !== undefined) merged.marginTop = `${top}px`
  if (wrap) merged.flexWrap = 'wrap'
  return (
    <div className={`tw-row ${className ?? ''}`} style={merged} onClick={onClick}>
      {children}
    </div>
  )
}

/// divider()：1px outline_variant（§4.3）
export function Divider({ top }: { top?: number }) {
  const style: CSSProperties | undefined = top !== undefined ? { marginTop: `${top}px` } : undefined
  return <div className="tw-divider" style={style} />
}
