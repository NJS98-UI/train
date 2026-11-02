/// 中文文案 helper：区域文案与固定提示语，全部按 ui-keepalive.md §2 原样照抄。

/// regionLabel（§2.2 末）：SG ? "国际服" : "国服"
export function regionLabel(regionId: string | null | undefined): string {
  return regionId === 'sg' ? '国际服' : '国服'
}

export function regionSupported(regionId: string | null | undefined): boolean {
  return regionId !== 'sg'
}

/// regionLabelFull = regionLabel +（不受支持时）"（未开发）"
export function regionLabelFull(regionId: string | null | undefined): string {
  return regionLabel(regionId) + (regionSupported(regionId) ? '' : '（未开发）')
}

/// TraeRegion.SG.unsupportedNote 原文（§2.1 卡片 3）
export const UNSUPPORTED_NOTE =
  '未开发：登录可通，但聊天不支持多轮上下文与外部工具调用；且免费额度极少（约 $1/月）。建议使用国服。'

/// 积分显示三态（§2.2 第 1 项 / 验收 §9.4）：`…` / `—` / `—（原因）`，数字整数不带小数、否则 .1f
export function creditFormat(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return '—'
  return Number.isInteger(n) ? String(n) : n.toFixed(1)
}

export function creditLabel(value: number | null | undefined, error?: string | null, loading?: boolean): string {
  if (loading) return '…'
  if (error) return `—（${error}）`
  return creditFormat(value)
}

/// 更新时间位（§2.2 第 1 项）：loading / 未查询 / `更新于 HH:mm:ss`
export function creditUpdatedLabel(loading: boolean, at: number): string {
  if (loading) return '正在查询全部账号…'
  if (at <= 0) return '尚未查询（点右上「刷新积分」）'
  const d = new Date(at)
  const p = (n: number) => String(n).padStart(2, '0')
  return `更新于 ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

/// HH:mm:ss（记录卡时间戳，§2.4）
export function hms(ts: number): string {
  const d = new Date(ts)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}
