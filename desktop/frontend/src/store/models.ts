// models 切片（§8.2）：models_view 返回 {loaded,items,counts}；modelsLoading 有 finally 复位。
import { create } from 'zustand'
import { call } from '../ipc'
import { toast } from './toast'

export interface ModelItem {
  id: string
  name?: string
  source?: 'trae' | 'qoder'
  multiplier?: number | null
  free?: boolean
  verified?: boolean
  context?: number | null
  aliases?: string[]
  ownedBy?: string
}

export interface ModelsView {
  loaded: boolean
  items: ModelItem[]
  counts?: { trae?: number; qoder?: number; total?: number }
}

interface ModelsSlice {
  view: ModelsView | null
  modelsLoading: boolean
  query: string
  error: string | null
  load: () => Promise<void>
  setQuery: (q: string) => void
  refreshTrae: () => Promise<void>
  syncQoder: () => Promise<void>
}

export const useModels = create<ModelsSlice>((set, get) => ({
  view: null,
  modelsLoading: false,
  query: '',
  error: null,

  setQuery(q) {
    // 过滤只改显隐，不重建列表（保住搜索框内容/焦点/滚动位置，§1.4 / §2.3）
    set({ query: q })
  },

  async load() {
    try {
      const view = await call<ModelsView>('models_view')
      set({ view, error: null })
    } catch (e) {
      set({ error: (e as Error).message })
    }
  },

  async refreshTrae() {
    if (get().modelsLoading) return
    set({ modelsLoading: true })
    toast('正在加载模型目录…')
    try {
      await call('models_refresh_trae')
      await get().load()
    } catch (e) {
      toast('拉取失败，请检查网络后重试')
      set({ error: (e as Error).message })
    } finally {
      set({ modelsLoading: false })
    }
  },

  async syncQoder() {
    if (get().modelsLoading) return
    set({ modelsLoading: true })
    toast('正在拉取 Qoder 官方模型目录…')
    try {
      await call('models_sync_qoder')
      await get().load()
    } catch (e) {
      toast('拉取失败，继续用内置模型')
      set({ error: (e as Error).message })
    } finally {
      set({ modelsLoading: false })
    }
  },
}))
