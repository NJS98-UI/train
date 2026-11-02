// Toast 栈：LENGTH_SHORT 等值 = 3s 自动消失（ui-keepalive.md §3 尾注 / §8.3）
import { create } from 'zustand'

export interface ToastItem {
  id: number
  text: string
}

interface ToastState {
  items: ToastItem[]
  push: (text: string) => void
  dismiss: (id: number) => void
}

let seq = 0

export const useToast = create<ToastState>((set, get) => ({
  items: [],
  push(text) {
    const id = ++seq
    set({ items: [...get().items, { id, text }] })
    setTimeout(() => get().dismiss(id), 3000)
  },
  dismiss(id) {
    set({ items: get().items.filter((t) => t.id !== id) })
  },
}))

/// 全局调用入口（等价 MainActivity.toast(...)）
export function toast(text: string): void {
  useToast.getState().push(text)
}
