// records 切片（§8.2）：records_get → {items, agg}（聚合语义同 UsageStore.aggregate）。
// 后端未实现时 call 抛错 → 页面 catch 后走空态占位。
import { create } from 'zustand'
import { call } from '../ipc'

export interface UsageRecord {
  ts: number
  model: string
  prompt: number
  completion: number
  total: number
  ms: number
  ok: boolean
  err: string
}

export interface ModelAgg {
  model: string
  calls: number
  okCalls: number
  prompt: number
  completion: number
  total: number
  ms: number
}

export interface RecordsView {
  items: UsageRecord[]
  agg: ModelAgg[]
}

interface RecordsSlice {
  data: RecordsView | null
  loading: boolean
  error: string | null
  load: () => Promise<void>
}

export const useRecords = create<RecordsSlice>((set) => ({
  data: null,
  loading: false,
  error: null,

  async load() {
    set({ loading: true })
    try {
      const data = await call<RecordsView>('records_get')
      set({ data, loading: false, error: null })
    } catch (e) {
      set({ data: null, loading: false, error: (e as Error).message })
    }
  },
}))
