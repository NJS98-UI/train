// nav 切片（§8.2）：currentPage + sub（'keepalive' | 'style' | null），对应 showKeepAlive/showStyle + STATE_SUB。
import { create } from 'zustand'

export type PageId = 'overview' | 'account' | 'models' | 'records'
export type SubPage = 'keepalive' | 'style'

/// 页签标题/副标题（§1.2 表原样）
export const PAGES: { id: PageId; title: string; subtitle: string }[] = [
  { id: 'overview', title: '概览', subtitle: '本地服务运行状态' },
  { id: 'account', title: '账号', subtitle: '账户登录状态' },
  { id: 'models', title: '模型', subtitle: '本地可用的模型表' },
  { id: 'records', title: '记录', subtitle: '调用用量与记录' },
]

/// 二级页标题/副标题（§1.3 原样；桌面版把「保活设置」重定义为「常驻设置」）
export const SUB_PAGES: Record<SubPage, { title: string; subtitle: string }> = {
  keepalive: { title: '保活设置', subtitle: '让服务在后台长期活着' },
  style: { title: '外观风格', subtitle: '配色与形态，随你挑' },
}

interface NavState {
  page: PageId
  sub: SubPage | null
  go: (page: PageId) => void
  openKeepAlive: () => void
  openStylePage: () => void
  back: () => void
}

export const useNav = create<NavState>((set) => ({
  page: 'overview',
  sub: null,
  // 点任意底部 Tab 即退出二级页（§1.3）
  go(page) {
    set({ page, sub: null })
  },
  // 进二级页不动底部高亮（§1.3）
  openKeepAlive() {
    set({ sub: 'keepalive' })
  },
  openStylePage() {
    set({ sub: 'style' })
  },
  // 两条返回出口都回概览（§1.3）
  back() {
    set({ sub: null, page: 'overview' })
  },
}))
