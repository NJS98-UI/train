//! 应用骨架（桌面横屏版）：左侧竖排导航（自上而下 概览/账号/模型/记录）→ 右侧 顶栏（二级页返回在标题上方）→ ScrollPage 内容 → Toast 栈。
//! 主题注入：按 {styleId, mode} 把 19 键 + 形态/字号 token 写进 :root；mode=system 用 matchMedia 监听。
import { useEffect, useLayoutEffect, useRef, type ReactNode } from 'react'
import { ScrollPage } from './components'
import { ToastStack } from './components/ToastStack'
import { OverviewPage } from './pages/OverviewPage'
import { AccountPage } from './pages/AccountPage'
import { ModelsPage } from './pages/ModelsPage'
import { RecordsPage } from './pages/RecordsPage'
import { KeepAlivePage } from './pages/KeepAlivePage'
import { StylePage } from './pages/StylePage'
import { PAGES, SUB_PAGES, useNav } from './store/nav'
import { useUiStyle } from './store/uiStyle'
import { useSettings } from './store/settings'
import { useService } from './store/service'
import { useKeepAlive } from './store/keepalive'
import { useTrae } from './store/trae'
import { subscribeEvents } from './store/events'
import { applyTheme, selfCheck, systemPrefersDark } from './theme/styles'

/// §7 导航图标（dashboard / account / models / history），跟随 currentColor 着色
const NAV_ICONS: Record<string, ReactNode> = {
  overview: (
    <svg className="tw-nav__icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <path d="M3 13h8V3H3v10zm0 8h8v-6H3v6zm10 0h8V11h-8v10zm0-18v6h8V3h-8z" />
    </svg>
  ),
  account: (
    <svg className="tw-nav__icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <path d="M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z" />
    </svg>
  ),
  models: (
    <svg className="tw-nav__icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <path d="M11.99 18.54l-.01-7.48 5.99-3.49-5.98-3.48-5.99 3.48 5.99 3.49.01 7.48zM12 22C6.48 22 2 17.52 2 12S6.48 2 12 2s10 4.48 10 10-4.48 10-10 10z" />
    </svg>
  ),
  records: (
    <svg className="tw-nav__icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <path d="M13 3a9 9 0 0 0-9 9H1l3.89 3.89.07.14L9 12H6a7 7 0 1 1 2.05 4.95l-1.42 1.42A9 9 0 1 0 13 3zm-1 5v5l4.28 2.54.72-1.21-3.5-2.08V8H12z" />
    </svg>
  ),
}

export default function App() {
  const styleId = useUiStyle((s) => s.styleId)
  const mode = useUiStyle((s) => s.mode)
  const prefersDark = useUiStyle((s) => s.prefersDark)
  const setPrefersDark = useUiStyle((s) => s.setPrefersDark)

  const page = useNav((s) => s.page)
  const sub = useNav((s) => s.sub)
  const go = useNav((s) => s.go)
  const openStylePage = useNav((s) => s.openStylePage)
  const back = useNav((s) => s.back)

  const rootRef = useRef<HTMLDivElement>(null)

  // 1) 主题变量注入（写入 documentElement，等价 AppStyle 运行期查表）
  useLayoutEffect(() => {
    applyTheme(document.documentElement, styleId, mode, prefersDark)
  }, [styleId, mode, prefersDark])

  // 2) mode=system：matchMedia 监听系统夜间切换（等价系统重建 Activity）
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    setPrefersDark(mq.matches)
    const onChange = (e: MediaQueryListEvent) => setPrefersDark(e.matches)
    mq.addEventListener('change', onChange)
    return () => mq.removeEventListener('change', onChange)
  }, [setPrefersDark])

  // 3) 启动：读配置（style 字段）→ 拉服务状态/体检/账号 → 订阅事件
  useEffect(() => {
    const cfg = useSettings.getState()
    const ui = useUiStyle.getState()

    // 首帧先按系统偏好落一次主题，避免闪白（systemPrefersDark 兜底）
    ui.setPrefersDark(systemPrefersDark())
    applyTheme(document.documentElement, ui.styleId, ui.mode, systemPrefersDark())

    void cfg.load().then((config) => {
      if (config) useUiStyle.getState().syncFromConfig(config.style)
    })
    void useService.getState().refresh()
    void useKeepAlive.getState().load()
    void useTrae.getState().load()

    const unsubscribe = subscribeEvents()
    const issues = selfCheck()
    if (issues.length > 0) console.warn('[AppStyle selfCheck]', issues)
    return unsubscribe
  }, [setPrefersDark])

  // 4) 键盘返回：处于二级页时回概览（§1.3 onBackPressed 等值）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && useNav.getState().sub) back()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [back])

  const meta = sub ? SUB_PAGES[sub] : PAGES.find((p) => p.id === page)!

  return (
    <div className="tw-shell" ref={rootRef}>
      <aside className="tw-rail">
        <div className="tw-rail__brand">
          <span className="tw-rail__logo">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3" />
              <path d="M9 12l2 2 4-4" />
            </svg>
          </span>
          <span className="tw-rail__name">TraeWork2API</span>
        </div>
        <nav className="tw-rail__nav">
          {PAGES.map((p) => {
            const active = !sub && p.id === page
            return (
              <button key={p.id} type="button" className={`tw-rail__item${active ? ' tw-rail__item--active' : ''}`} onClick={() => go(p.id)}>
                <span className="tw-rail__icon">{NAV_ICONS[p.id]}</span>
                <span className="tw-rail__label">{p.title}</span>
              </button>
            )
          })}
        </nav>
        <div className="tw-rail__foot">
          <button type="button" className={`tw-rail__item${sub === 'style' ? ' tw-rail__item--active' : ''}`} onClick={() => (sub === 'style' ? back() : openStylePage())}>
            <span className="tw-rail__icon">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" aria-hidden>
                <circle cx="13.5" cy="6.5" r={2.5} /><circle cx={17.5} cy={10.5} r={2.5} /><circle cx={8.5} cy={7.5} r={2.5} /><circle cx={6.5} cy={12.5} r={2.5} />
                <path d="M12 22a10 10 0 1 1 10-10c0 1.7-1.3 3-3 3h-2a2 2 0 0 0-2 3 2 2 0 0 1-2 3z" />
              </svg>
            </span>
            <span className="tw-rail__label">外观</span>
          </button>
        </div>
      </aside>

      <div className="tw-app">
        <header className="tw-topbar">
          <div className="tw-topbar__inner">
            {sub && (
              <button type="button" className="tw-topbar__back" onClick={back}>
                ← 返回
              </button>
            )}
            <div className="tw-topbar__title">{meta.title}</div>
            <div className="tw-topbar__subtitle">{meta.subtitle}</div>
          </div>
        </header>
        <div className="tw-divider" style={{ margin: 0 }} />

        <main className="tw-content">
          <ScrollPage>
            {sub === 'keepalive' ? (
              <KeepAlivePage />
            ) : sub === 'style' ? (
              <StylePage />
            ) : page === 'overview' ? (
              <OverviewPage />
            ) : page === 'account' ? (
              <AccountPage />
            ) : page === 'models' ? (
              <ModelsPage />
            ) : (
              <RecordsPage />
            )}
          </ScrollPage>
        </main>

        <ToastStack />
      </div>
    </div>
  )
}
