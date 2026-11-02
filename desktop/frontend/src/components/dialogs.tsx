// 弹窗原子（§8.1 / §2.7）：ConfirmDialog 为唯一弹窗壳，PickerDialog / TimePicker / DiagnoseDialog 基于它。
// 自绘面板：cardPad 内边距竖排；title FS_CARD_TITLE bold；message FS_VALUE on_surface_variant；按钮行等宽。
import { useEffect, useState, type ReactNode } from 'react'
import { ActionButton } from './buttons'
import { SIZES, cssVar } from '../theme/styles'
import { copyText } from '../ipc'
import { toast } from '../store/toast'

function Overlay({ children, onBackdrop }: { children: ReactNode; onBackdrop?: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onBackdrop?.()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onBackdrop])
  return (
    <div className="tw-overlay" onClick={onBackdrop}>
      <div className="tw-dialog" onClick={(e) => e.stopPropagation()}>
        {children}
      </div>
    </div>
  )
}

export interface ConfirmDialogProps {
  title: string
  message?: string
  okText?: string
  cancelText?: string
  danger?: boolean
  content?: ReactNode
  onOk?: () => void
  onCancel?: () => void
}

export function ConfirmDialog({ title, message, okText = '确定', cancelText = '取消', danger, content, onOk, onCancel }: ConfirmDialogProps) {
  return (
    <Overlay onBackdrop={onCancel}>
      <div className="tw-dialog__title">{title}</div>
      {message && <div className="tw-dialog__message">{message}</div>}
      {content && <div className="tw-dialog__content">{content}</div>}
      <div className="tw-dialog__actions">
        <ActionButton variant="outlined" onClick={onCancel}>
          {cancelText}
        </ActionButton>
        <ActionButton variant={danger ? 'danger' : 'filled'} onClick={onOk}>
          {okText}
        </ActionButton>
      </div>
    </Overlay>
  )
}

export interface PickerRow {
  key: string
  primary: ReactNode
  caption?: ReactNode
  current?: boolean
  onDelete?: () => void
  onClick?: () => void
}

/// showPickerDialog()：行高固定 56dp，可见高度 = min(rows, 5) × 56dp，内部滚动，只有「关闭」键（§2.2 末）
export function PickerDialog({ title, rows, onClose }: { title: string; rows: PickerRow[]; onClose: () => void }) {
  const visible = Math.min(rows.length, SIZES.pickerVisibleRows)
  const maxHeight = visible * SIZES.pickerRowHeight + (visible > 0 ? visible - 1 : 0) * SIZES.dividerHeight + 2
  return (
    <Overlay onBackdrop={onClose}>
      <div className="tw-dialog__title">{title}</div>
      <div className="tw-picker" style={{ maxHeight, background: cssVar('surface') }}>
        {rows.map((row, i) => (
          <div key={row.key}>
            {i > 0 && <div className="tw-divider" />}
            <div className="tw-picker__row" onClick={row.onClick}>
              <div className="tw-picker__main">
                <div className="tw-picker__primary">{row.primary}</div>
                {row.caption && <div className="tw-picker__caption">{row.caption}</div>}
              </div>
              {row.current && <span className="tw-picker__check">✓</span>}
              {row.onDelete && (
                <button type="button" className="tw-picker__delete" onClick={(e) => { e.stopPropagation(); row.onDelete?.() }}>
                  删除
                </button>
              )}
            </div>
          </div>
        ))}
      </div>
      <div className="tw-dialog__actions">
        <ActionButton variant="outlined" onClick={onClose}>
          关闭
        </ActionButton>
      </div>
    </Overlay>
  )
}

const pad2 = (n: number) => String(n).padStart(2, '0')

function Wheel({ label, value, onBump }: { label: string; value: number; onBump: (delta: number) => void }) {
  return (
    <div className="tw-timepicker__col">
      <button type="button" className="tw-timepicker__arrow" onClick={() => onBump(1)} aria-label={`${label}加一`}>
        ▲
      </button>
      <div className="tw-timepicker__value">{pad2(value)}</div>
      <button type="button" className="tw-timepicker__arrow" onClick={() => onBump(-1)} aria-label={`${label}减一`}>
        ▼
      </button>
      <div className="tw-timepicker__hint">{label}</div>
    </div>
  )
}

/// NumberPicker(0-23 / 0-59, wrap) 的桌面等值：两枚滚轮 + 冒号，输出 `HH:mm`（§2.2 第 7 项）
export function TimePicker({ value, onChange }: { value: string; onChange: (hhmm: string) => void }) {
  const [hh, mm] = value.split(':')
  const hour = Math.min(23, Math.max(0, Number.parseInt(hh ?? '0', 10) || 0))
  const minute = Math.min(59, Math.max(0, Number.parseInt(mm ?? '0', 10) || 0))
  const wrap = (base: number, delta: number, span: number) => (base + delta + span) % span
  return (
    <div className="tw-timepicker">
      <Wheel label="时 0-23" value={hour} onBump={(d) => onChange(`${pad2(wrap(hour, d, 24))}:${pad2(minute)}`)} />
      <span className="tw-timepicker__colon">:</span>
      <Wheel label="分 0-59" value={minute} onBump={(d) => onChange(`${pad2(hour)}:${pad2(wrap(minute, d, 60))}`)} />
    </div>
  )
}

/// 定时领取时间弹窗（promptCustomTime）：TimePicker + 取消/确定，默认时刻 09:00
export function TimePickerDialog({
  title = '定时领取时间',
  value = '09:00',
  onOk,
  onCancel,
}: {
  title?: string
  value?: string
  onOk: (hhmm: string) => void
  onCancel: () => void
}) {
  const [draft, setDraft] = useState(value)
  return (
    <Overlay onBackdrop={onCancel}>
      <div className="tw-dialog__title">{title}</div>
      <div className="tw-dialog__content">
        <TimePicker value={draft} onChange={setDraft} />
      </div>
      <div className="tw-dialog__actions">
        <ActionButton variant="outlined" onClick={onCancel}>
          取消
        </ActionButton>
        <ActionButton variant="filled" onClick={() => onOk(draft)}>
          确定
        </ActionButton>
      </div>
    </Overlay>
  )
}

/// 诊断弹窗（§2.2 第 7 项）：等宽、可选中、可复制
export async function copyDiagnose(text: string): Promise<void> {
  try {
    await copyText(text)
    toast('已复制')
  } catch (e) {
    toast((e as Error).message)
  }
}

export function DiagnoseDialog({ title, text, onClose }: { title: string; text: string; onClose: () => void }) {
  return (
    <Overlay onBackdrop={onClose}>
      <div className="tw-dialog__title">{title}</div>
      <div className="tw-dialog__content">
        <pre className="tw-diagnose">{text}</pre>
      </div>
      <div className="tw-dialog__actions">
        <ActionButton variant="outlined" onClick={() => void copyDiagnose(text)}>
          复制
        </ActionButton>
        <ActionButton variant="filled" onClick={onClose}>
          关闭
        </ActionButton>
      </div>
    </Overlay>
  )
}
