// Toast 栈（§8.1 / §3）：顶部居中，3s 自动消失（时长在 useToast 内部计时）。
import { useToast } from '../store/toast'

export function ToastStack() {
  const items = useToast((s) => s.items)
  const dismiss = useToast((s) => s.dismiss)
  if (items.length === 0) return null
  return (
    <div className="tw-toasts" role="status" aria-live="polite">
      {items.map((t) => (
        <div key={t.id} className="tw-toast" onClick={() => dismiss(t.id)}>
          {t.text}
        </div>
      ))}
    </div>
  )
}
