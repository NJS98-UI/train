// 统一复制入口（Android copyToClipboard + toast 文案成对出现，§2.x / §3）
import { copyText } from '../ipc'
import { toast } from '../store/toast'

export async function copyWithToast(text: string, message: string): Promise<void> {
  try {
    await copyText(text)
    toast(message)
  } catch (e) {
    toast(`复制失败：${(e as Error).message}`)
  }
}
