// IPC 适配层：Tauri 内走 invoke('ipc')；浏览器预览走 /api/invoke/{cmd} + SSE /api/events。
// 命令名与出入参是前后端契约（design.md §IPC 契约），任何改动两端同步。
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

const isTauri = typeof (window as any).__TAURI_INTERNALS__ !== 'undefined'

interface IpcResp<T> { ok: boolean; data?: T; error?: string }

/// 事件订阅的取消函数（onEvent 返回值）
export type Unlisten = () => void

export async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  let resp: IpcResp<T>
  if (isTauri) {
    resp = await invoke<IpcResp<T>>('ipc', { cmd, args })
  } else {
    const r = await fetch(`/api/invoke/${cmd}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(args ?? {}),
    })
    // 桥接未启动时 vite proxy 会返回非 JSON 的 5xx 页面，这里给一句可读的占位原因
    const text = await r.text()
    try {
      resp = JSON.parse(text) as IpcResp<T>
    } catch {
      throw new Error('本地 IPC 桥接未启动')
    }
  }
  if (!resp.ok) throw new Error(resp.error ?? 'IPC 调用失败')
  return resp.data as T
}

export function onEvent(topic: string, cb: (payload: any) => void): () => void {
  if (isTauri) {
    let un: (() => void) | null = null
    let dead = false
    listen(topic, (e) => cb(e.payload)).then((f) => { if (dead) f(); else un = f })
    return () => { dead = true; un?.() }
  }
  const es = new EventSource('/api/events')
  es.addEventListener(topic, (e: MessageEvent) => {
    try { cb(JSON.parse(e.data)) } catch { /* ignore */ }
  })
  return () => es.close()
}

export function openExternal(url: string): void {
  if (isTauri) {
    import('@tauri-apps/plugin-opener').then(({ openUrl }) => openUrl(url))
  } else {
    window.open(url, '_blank')
  }
}

export async function copyText(text: string): Promise<void> {
  if (isTauri) {
    const { writeText } = await import('@tauri-apps/plugin-clipboard-manager')
    await writeText(text)
  } else {
    await navigator.clipboard.writeText(text)
  }
}
